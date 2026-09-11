//! The mirror-watcher — issue #299, the actual acting-as-a-mirror piece of
//! #40's decided design. #211 (closed) already built the read-side API
//! (`GET /ledger/sth/latest`, `GET /ledger/sth/{tree_size}`, `GET
//! /ledger/proof/consistency`, `GET /ledger/proof/inclusion`) and this
//! ticket adds `GET /ledger/entries` (`crate::settlement::list_entries`);
//! this module is what actually calls those endpoints against one or more
//! configured peers, verifies what comes back, and stores it.
//!
//! **Lives in-process inside `avalon-server`, as a background task spawned
//! from `main.rs`** (this ticket's own open implementation call, decided
//! here) — the same shape `crate::outbox::run_worker` and
//! `crate::retention::run_worker` already use: a `tokio::spawn`ed loop,
//! gated by whether the relevant env config is even set, sharing the
//! process's existing `PgPool`/`PostgresSettlementProvider` rather than
//! standing up a second connection or a separate binary. A `avalon
//! mirror-watch <peer-url>` CLI subcommand was the other option the ticket
//! left open, but it would need its own Postgres pool setup, its own
//! migration-running, and its own long-lived-process lifecycle (`avalon`
//! today is a short-lived diagnostic tool — `inspect-ledger`,
//! `outbox-status` — that runs once and exits, not a daemon) duplicating
//! what `avalon-server` already has. A "2 Settlement nodes" topology is two
//! `avalon-server` deployments, each against its own Postgres — a "mirror"
//! is just one of them started with `AVALON_MIRROR_PEERS` pointed at the
//! other; nothing here requires the watching node to be a pure mirror with
//! no writes of its own.
//!
//! **Multi-peer by design, not just multi-peer-configurable.**
//! `AVALON_MIRROR_PEERS` accepts more than one URL, and every configured
//! peer is actually used, not just the first one that answers:
//!
//! - Every tick, **every** configured peer is polled independently for its
//!   latest STH ([`watch_peer_once`]) — one peer being unreachable or
//!   misbehaving never stops the others from being watched, and every
//!   peer's observation is stored and equivocation-checked against every
//!   *other* observation this node has ever recorded for that
//!   `network_id`/`tree_size`, from any source (`avalon_chain::mirror`'s
//!   `observed_sths` is not scoped per peer).
//! - Peers are then grouped by `network_id` and handed to
//!   [`backfill_network`], which (a) refuses to extend a network's mirrored
//!   history past any `tree_size` with an already-recorded, unresolved
//!   equivocation finding — #299 owns *detection*, not automatic
//!   resolution, and there is no correct automatic pick between two validly
//!   signed but disagreeing STHs — and (b) picks the tree head **this
//!   tick's peers most widely agree on** (a majority-agreement gate, not
//!   "whichever peer answered first") before trusting it for backfill.
//! - Backfill itself ([`backfill`]) round-robins across every peer that
//!   corroborated the chosen tree head: if the peer a given page/proof
//!   request lands on is unreachable, the next one is tried before giving
//!   up for that tick. Content storage
//!   (`avalon_chain::mirror::insert_mirrored_entry`) is keyed on
//!   `(network_id, seq)`, not per peer, so failing over mid-backfill never
//!   duplicates or restarts progress — any configured peer of the same
//!   network is an interchangeable source of the same
//!   independently-verified content.
//!
//! This is deliberately not exhaustive N-way verification of every single
//! entry against every peer (that would multiply request volume by the
//! peer count for no additional safety once one inclusion proof has
//! already verified against the trusted root) — it is real multi-source
//! resilience and real cross-peer corroboration before trust is extended,
//! not a single hardcoded peer.
//!
//! **Never trusts unverified content.** A signature failure, a proof that
//! doesn't verify, or a returned `root_hash` that doesn't match the
//! already-verified STH aborts that attempt (logged, never panicked on,
//! falling over to the next peer where one is available) rather than
//! accepting anything — see each function's own doc comment for exactly
//! where that boundary is.

use std::collections::HashMap;
use std::time::Duration;

use avalon_chain::mirror::{self, ObservedSth, SELF_SIGNED_SOURCE};
use avalon_chain::sth::{self, SignedTreeHead};
use avalon_chain::{merkle, PostgresSettlementProvider};
use avalon_indexer::postgres::PostgresIndexer;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::GlobalId;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use sqlx::Acquire;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

/// How many entries to request per bulk-entries page while backfilling.
const BACKFILL_PAGE_SIZE: i64 = 200;

pub struct MirrorWatcherConfig {
    /// Base URLs of peers to watch, e.g. `http://localhost:8081` — no
    /// trailing slash (stripped in [`Self::from_env`] if present). More
    /// than one is a normal, supported configuration — see module docs.
    pub peers: Vec<String>,
    pub poll_interval: Duration,
}

impl MirrorWatcherConfig {
    /// `AVALON_MIRROR_PEERS` — comma-separated peer base URLs. Unset or
    /// empty means this node isn't watching anyone; `None` here is the
    /// signal `main.rs` uses to skip spawning the watcher entirely, the
    /// same "only spawn if configured" pattern
    /// `retention::RetentionConfig::should_prune` already uses.
    ///
    /// Poll interval defaults to 30s, overridable via
    /// `AVALON_MIRROR_POLL_INTERVAL_SECS` — an STH is a few hundred bytes
    /// of JSON, so bandwidth isn't the constraint; this is node-to-node
    /// traffic only (never touches end-user clients), and 30-60s is a
    /// sensible default cadence rather than a tight poll loop.
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("AVALON_MIRROR_PEERS").ok()?;
        let peers: Vec<String> = raw
            .split(',')
            .map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if peers.is_empty() {
            return None;
        }
        let poll_interval = std::env::var("AVALON_MIRROR_POLL_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(30));
        Some(Self {
            peers,
            poll_interval,
        })
    }
}

/// Spawned once at startup (see `main.rs`) when [`MirrorWatcherConfig::from_env`]
/// returns `Some`. Never returns. See module docs for the two-phase
/// per-tick shape: every peer is polled for its STH independently first
/// (phase 1), then peers are grouped by `network_id` and backfilled
/// together (phase 2) — one unreachable/misbehaving peer never stops the
/// others from being watched or from covering for it during backfill.
pub async fn run_worker(
    pool: PgPool,
    chain: PostgresSettlementProvider,
    indexer: PostgresIndexer,
    config: MirrorWatcherConfig,
) {
    let verify_key = match sth::load_verify_key_from_env() {
        Ok(key) => key,
        Err(err) => {
            eprintln!(
                "mirror-watcher: cannot start, failed to load AVALON_SETTLEMENT_VERIFY_KEY: {err}"
            );
            return;
        }
    };
    let client = reqwest::Client::new();

    println!(
        "mirror-watcher: watching {} peer(s) every {:?}: {}",
        config.peers.len(),
        config.poll_interval,
        config.peers.join(", ")
    );

    loop {
        // Phase 1: poll every peer independently for its latest STH,
        // verify + store + equivocation-check each one. Peers that
        // succeed are grouped by the network_id they reported, since
        // that's what phase 2 backfills over — usually every configured
        // peer is the same network, but nothing here assumes it.
        let mut verified_by_network: HashMap<String, Vec<(String, SignedTreeHead)>> =
            HashMap::new();
        for peer in &config.peers {
            match fetch_and_verify_sth(&client, &verify_key, peer).await {
                Ok(sth) => {
                    let observed = ObservedSth::from_sth(peer, &sth, OffsetDateTime::now_utc());
                    match mirror::insert_observation(&pool, &observed).await {
                        Ok(is_new) => {
                            if is_new {
                                if let Err(err) = check_equivocation(&pool, &chain, &observed).await
                                {
                                    eprintln!("mirror-watcher: {peer}: {err}");
                                }
                            }
                            verified_by_network
                                .entry(sth.network_id.clone())
                                .or_default()
                                .push((peer.clone(), sth));
                        }
                        Err(err) => eprintln!("mirror-watcher: {peer}: {err}"),
                    }
                }
                Err(err) => eprintln!("mirror-watcher: {peer}: {err}"),
            }
        }

        // Phase 2: for each network at least one peer reported this tick,
        // pick a corroborated tree head and backfill against every peer
        // that agreed on it.
        for (network_id, observations) in &verified_by_network {
            if let Err(err) =
                backfill_network(&client, &pool, &indexer, network_id, observations).await
            {
                eprintln!("mirror-watcher: {network_id}: {err}");
            }
        }

        tokio::time::sleep(config.poll_interval).await;
    }
}

#[derive(Debug, thiserror::Error)]
pub enum MirrorWatcherError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("peer returned an unparseable response: {0}")]
    Decode(String),
    #[error("STH signature verification failed — refusing to trust this observation")]
    InvalidSignature,
    #[error("inclusion proof failed verification for seq={seq} against tree_size={tree_size}")]
    InvalidInclusionProof { seq: i64, tree_size: i64 },
    #[error("peer's inclusion-proof root_hash did not match the already-verified STH root_hash")]
    RootHashMismatch,
    #[error("every configured peer for this network failed this request")]
    AllPeersFailed,
    #[error("storage error: {0}")]
    Storage(#[from] avalon_chain::SettlementError),
}

#[derive(Deserialize)]
struct SignedTreeHeadDto {
    tree_size: i64,
    root_hash: String,
    network_id: String,
    signing_key_id: String,
    signature: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

impl From<SignedTreeHeadDto> for SignedTreeHead {
    fn from(dto: SignedTreeHeadDto) -> Self {
        SignedTreeHead {
            tree_size: dto.tree_size,
            root_hash: dto.root_hash,
            network_id: dto.network_id,
            signing_key_id: dto.signing_key_id,
            signature: dto.signature,
            created_at: dto.created_at,
        }
    }
}

#[derive(Deserialize)]
struct LedgerEntryDto {
    seq: i64,
    event_id: Uuid,
    kind: String,
    issuer: String,
    subject: String,
    payload: Option<serde_json::Value>,
    #[serde(default)]
    #[allow(dead_code)] // carried through the response, not needed once verified/stored
    payload_pruned: bool,
    version: i32,
    #[serde(with = "time::serde::rfc3339")]
    event_timestamp: OffsetDateTime,
    prev_hash: String,
    entry_hash: String,
    batch_id: Uuid,
}

#[derive(Deserialize)]
struct InclusionProofDto {
    root_hash: String,
    leaf_hash: String,
    proof: Vec<String>,
}

/// Fetches `peer`'s latest STH and verifies its signature — the one step
/// every peer goes through in phase 1, regardless of what happens next.
async fn fetch_and_verify_sth(
    client: &reqwest::Client,
    verify_key: &VerifyingKey,
    peer: &str,
) -> Result<SignedTreeHead, MirrorWatcherError> {
    let sth = fetch_latest_sth(client, peer).await?;
    if !sth::verify_tree_head(verify_key, &sth) {
        eprintln!(
            "mirror-watcher: {peer}: STH signature verification FAILED for tree_size={} — not storing, not trusting",
            sth.tree_size
        );
        return Err(MirrorWatcherError::InvalidSignature);
    }
    Ok(sth)
}

async fn fetch_latest_sth(
    client: &reqwest::Client,
    peer: &str,
) -> Result<SignedTreeHead, MirrorWatcherError> {
    let url = format!("{peer}/ledger/sth/latest");
    let dto: SignedTreeHeadDto = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(|e| MirrorWatcherError::Decode(e.to_string()))?;
    Ok(dto.into())
}

/// Compares `observed` against every other observation this node has
/// recorded at the same `network_id`/`tree_size` — from other peers *and*
/// (if this node is also a Settlement authority) its own signed history —
/// and durably records/logs any disagreement found. See
/// `avalon_chain::mirror`'s module docs for the surfacing mechanism. This
/// ticket owns detection only: there is no "pick the correct STH" logic
/// here or anywhere else in this module — both STHs in a real equivocation
/// are validly signed, so there is no automatically-correct answer.
async fn check_equivocation(
    pool: &PgPool,
    chain: &PostgresSettlementProvider,
    observed: &ObservedSth,
) -> Result<(), MirrorWatcherError> {
    let mut existing =
        mirror::observations_at(pool, &observed.network_id, observed.tree_size).await?;

    // Fold in this node's own signed history, if it has one at this exact
    // tree_size — a node that is both an authority and a mirror must catch
    // itself disagreeing with what it broadcasts, not just catch two peers
    // disagreeing with each other.
    if chain.network_id() == observed.network_id {
        if let Some(own) = chain.signed_tree_head_at(observed.tree_size).await? {
            existing.push(ObservedSth::from_sth(
                SELF_SIGNED_SOURCE,
                &own,
                own.created_at,
            ));
        }
    }

    for finding in mirror::detect_equivocation(&existing, observed) {
        mirror::record_equivocation(pool, &finding).await?;
    }

    Ok(())
}

/// Phase 2 for one `network_id`: decide whether — and against what tree
/// head — to backfill this tick, given every peer that successfully
/// reported an STH for this network this tick.
///
/// Two gates, both before a single byte of entry content is trusted:
///
/// 1. **Equivocation gate.** If this network already has *any* recorded
///    equivocation finding, backfill refuses to proceed at all. This
///    ticket owns detection, not resolution — once two validly signed
///    STHs disagree for this network, there is no automatically-correct
///    tree to keep extending, and continuing to backfill against whichever
///    peer happens to answer would silently pick a side. A human has to
///    resolve this (tracked as a separate follow-up); this node just stops
///    advancing until that happens.
/// 2. **Corroboration gate.** Among this tick's successfully-polled peers,
///    pick the `(tree_size, root_hash)` the largest number of them agree
///    on — not simply whichever peer happened to be first in the config
///    list. A peer reporting a different, smaller `tree_size` is just
///    behind, not disagreeing, and isn't counted against the winner.
async fn backfill_network(
    client: &reqwest::Client,
    pool: &PgPool,
    indexer: &PostgresIndexer,
    network_id: &str,
    observations: &[(String, SignedTreeHead)],
) -> Result<(), MirrorWatcherError> {
    let equivocations = mirror::unresolved_equivocations(pool, network_id).await?;
    if !equivocations.is_empty() {
        eprintln!(
            "mirror-watcher: {network_id}: refusing to backfill — {} unresolved equivocation finding(s) recorded for this network; this needs human investigation before further backfill can be trusted",
            equivocations.len()
        );
        return Ok(());
    }

    let mut agreement: HashMap<(i64, &str), Vec<&str>> = HashMap::new();
    for (peer, sth) in observations {
        agreement
            .entry((sth.tree_size, sth.root_hash.as_str()))
            .or_default()
            .push(peer.as_str());
    }
    let Some(((target_tree_size, target_root_hash), agreeing_peers)) =
        agreement.into_iter().max_by_key(|(_, peers)| peers.len())
    else {
        return Ok(());
    };
    if agreeing_peers.len() > 1 {
        println!(
            "mirror-watcher: {network_id}: tree_size={target_tree_size} corroborated by {} of {} polled peer(s)",
            agreeing_peers.len(),
            observations.len()
        );
    }

    let target_sth = observations
        .iter()
        .find(|(_, s)| s.tree_size == target_tree_size && s.root_hash == target_root_hash)
        .map(|(_, s)| s.clone())
        .expect("target_tree_size/target_root_hash were derived from this exact list");

    let candidate_peers: Vec<String> = agreeing_peers.iter().map(|p| p.to_string()).collect();
    backfill(client, pool, indexer, &candidate_peers, &target_sth).await
}

/// Fetches and independently verifies every entry between what's already
/// been mirrored locally and `sth.tree_size`, storing only entries whose
/// inclusion actually checks out. Round-robins across `candidate_peers`
/// (every peer that corroborated `sth` this tick, per
/// [`backfill_network`]) for each individual request — if one is
/// unreachable mid-backfill, the next candidate is tried before giving up
/// for this tick, rather than aborting outright. Storage is keyed on
/// `(network_id, seq)`, so failing over between peers never duplicates or
/// restarts progress (see `avalon_chain::mirror::insert_mirrored_entry`'s
/// doc comment).
///
/// Aborts (without storing anything further this pass) the moment *every*
/// candidate peer's response fails to verify for a given entry — the
/// invariant this ticket calls out explicitly: never trust unverified
/// content from a peer, and a partial-but-unverifiable backfill is worse
/// than simply retrying next tick.
///
/// Issue #313: once an entry's inclusion is verified, it's decoded into the
/// same `ProtocolEvent` shape `outbox::drain_once` builds and applied to
/// this node's own local `PostgresIndexer` — in the same transaction as the
/// `mirrored_entries` write, so a remote-settlement node's local reads are
/// fed *only* by content this node independently verified itself, never by
/// trusting a peer's response or (for the write side) `POST
/// /ledger/submit`'s request body.
async fn backfill(
    client: &reqwest::Client,
    pool: &PgPool,
    indexer: &PostgresIndexer,
    candidate_peers: &[String],
    sth: &SignedTreeHead,
) -> Result<(), MirrorWatcherError> {
    if candidate_peers.is_empty() {
        return Ok(());
    }

    let mut progress = mirror::mirrored_progress(pool, &sth.network_id).await?;
    if progress.verified_count >= sth.tree_size {
        return Ok(());
    }

    let expected_root = hex::decode(&sth.root_hash)
        .map_err(|e| MirrorWatcherError::Decode(format!("STH root_hash not valid hex: {e}")))?;
    let mut root = [0u8; 32];
    root.copy_from_slice(&expected_root);

    // Rotating start index so repeated ticks don't always hammer the same
    // first candidate — simple round-robin, not load-aware.
    let mut peer_cursor = 0usize;

    loop {
        if progress.verified_count >= sth.tree_size {
            break;
        }

        let (page, used_peer) = match fetch_entries_from_any(
            client,
            candidate_peers,
            &mut peer_cursor,
            progress.last_seq,
            BACKFILL_PAGE_SIZE,
        )
        .await
        {
            Ok(result) => result,
            Err(MirrorWatcherError::AllPeersFailed) => {
                eprintln!(
                        "mirror-watcher: {}: every candidate peer failed to serve entries since_seq={} — retrying next tick",
                        sth.network_id, progress.last_seq
                    );
                break;
            }
            Err(err) => return Err(err),
        };
        if page.is_empty() {
            // The peer(s) don't (yet) have as many entries as the
            // corroborated STH claims — a legitimate race (STH observed
            // slightly ahead of the entries page becoming visible) rather
            // than an error; pick up the rest on a later tick.
            break;
        }

        for entry in page {
            if progress.verified_count >= sth.tree_size {
                break;
            }

            // The leaf's rank among all committed entries — a running
            // count of entries this node has already verified and
            // accepted, matching how the authority side ranks entries
            // (`leaf_index_for_seq`/`entry_hashes_up_to`, see
            // `crates/chain/src/postgres.rs`'s module doc comment). This
            // only stays correct because backfill always proceeds
            // contiguously from `progress.last_seq` with no gaps skipped,
            // regardless of which candidate peer actually served it.
            let leaf_index = progress.verified_count as usize;

            let proof_dto = match fetch_inclusion_proof_from_any(
                client,
                candidate_peers,
                &mut peer_cursor,
                entry.seq,
                sth.tree_size,
            )
            .await
            {
                Ok(dto) => dto,
                Err(MirrorWatcherError::AllPeersFailed) => {
                    eprintln!(
                        "mirror-watcher: {}: every candidate peer failed to serve an inclusion proof for seq={} — retrying next tick",
                        sth.network_id, entry.seq
                    );
                    return Ok(());
                }
                Err(err) => return Err(err),
            };

            if proof_dto.root_hash != sth.root_hash {
                eprintln!(
                    "mirror-watcher: {}: inclusion-proof root_hash for seq={} did not match the already-verified/corroborated STH root_hash at tree_size={} — aborting backfill this tick",
                    sth.network_id, entry.seq, sth.tree_size
                );
                return Err(MirrorWatcherError::RootHashMismatch);
            }
            if proof_dto.leaf_hash != entry.entry_hash {
                eprintln!(
                    "mirror-watcher: {} (via {used_peer}): inclusion-proof leaf_hash for seq={} did not match the entry content fetched from GET /ledger/entries — aborting backfill this tick",
                    sth.network_id, entry.seq
                );
                return Err(MirrorWatcherError::InvalidInclusionProof {
                    seq: entry.seq,
                    tree_size: sth.tree_size,
                });
            }

            let leaf_bytes = hex::decode(&entry.entry_hash).map_err(|e| {
                MirrorWatcherError::Decode(format!("entry_hash not valid hex: {e}"))
            })?;
            let proof_nodes = decode_proof_nodes(&proof_dto.proof)?;

            let verified = merkle::verify_inclusion_proof(
                &leaf_bytes,
                leaf_index,
                sth.tree_size as usize,
                &proof_nodes,
                &root,
            );
            if !verified {
                eprintln!(
                    "mirror-watcher: {}: inclusion proof did NOT verify for seq={} (leaf_index={leaf_index}) against tree_size={} — refusing to accept, aborting backfill this tick",
                    sth.network_id, entry.seq, sth.tree_size
                );
                return Err(MirrorWatcherError::InvalidInclusionProof {
                    seq: entry.seq,
                    tree_size: sth.tree_size,
                });
            }

            let mirrored_entry = mirror::MirroredEntry {
                source_url: used_peer.clone(),
                network_id: sth.network_id.clone(),
                seq: entry.seq,
                event_id: entry.event_id,
                kind: entry.kind,
                issuer: entry.issuer,
                subject: entry.subject,
                payload: entry.payload,
                event_timestamp: entry.event_timestamp,
                version: entry.version,
                prev_hash: entry.prev_hash,
                entry_hash: entry.entry_hash,
                batch_id: entry.batch_id,
                verified_tree_size: sth.tree_size,
            };
            let protocol_event = protocol_event_from_mirrored(&mirrored_entry);

            // The `mirrored_entries` write always lands — that's the
            // cryptographically-verified fact this node is recording, and
            // it must never be held hostage by a projection failure.
            // Applying the event to the local indexer is best-effort on
            // top of it, via a SAVEPOINT: most projections assume core
            // rows a normal in-process write creates directly (outside the
            // outbox/ledger entirely, e.g. `identities`), so a replay-only
            // node can hit an FK a live write never would (`identity.created`
            // handled via `ensure_identity_row_exists` below; other kinds
            // may hit the same class of gap — tracked as a known
            // limitation, not chased further here). A projection failure
            // rolls back only its own savepoint and is logged loudly —
            // never aborts the outer commit, never blocks this node's
            // backfill/verification progress on every later entry forever.
            let mut tx = pool
                .begin()
                .await
                .map_err(|e| avalon_chain::SettlementError::Storage(e.to_string()))?;
            mirror::insert_mirrored_entry(&mut *tx, &mirrored_entry).await?;
            if let Some(event) = &protocol_event {
                let mut savepoint = tx.begin().await.map_err(|e: sqlx::Error| {
                    avalon_chain::SettlementError::Storage(e.to_string())
                })?;
                ensure_identity_row_exists(&mut savepoint, event).await?;
                match indexer.apply_in_tx(&mut savepoint, event).await {
                    Ok(()) => {
                        savepoint
                            .commit()
                            .await
                            .map_err(|e| avalon_chain::SettlementError::Storage(e.to_string()))?;
                    }
                    Err(err) => {
                        savepoint
                            .rollback()
                            .await
                            .map_err(|e| avalon_chain::SettlementError::Storage(e.to_string()))?;
                        eprintln!(
                            "mirror-watcher: {}: seq={} verified and mirrored, but the local indexer projection failed ({err}) — likely a core row this replay-only node never independently created; entry is stored, indexer state for it is incomplete",
                            sth.network_id, mirrored_entry.seq
                        );
                    }
                }
            } else {
                eprintln!(
                    "mirror-watcher: {}: seq={} could not be decoded into a ProtocolEvent (pruned payload or malformed issuer/subject) — mirrored, but not applied to the local indexer",
                    sth.network_id, mirrored_entry.seq
                );
            }
            tx.commit()
                .await
                .map_err(|e| avalon_chain::SettlementError::Storage(e.to_string()))?;

            progress.last_seq = mirrored_entry.seq;
            progress.verified_count += 1;
        }
    }

    Ok(())
}

/// Decodes a verified `MirroredEntry` into the same `ProtocolEvent` shape
/// `outbox::drain_once` builds from `protocol_outbox` rows — issue #313.
/// `None` for a pruned payload (issue #208 retention — the entry is still
/// mirrored, just not applicable to the indexer) or an `issuer`/`subject`
/// that isn't a well-formed `GlobalId`, which should never happen for a
/// genuine ledger entry but is handled as a skip, not a panic, since this is
/// peer-derived content.
fn protocol_event_from_mirrored(entry: &mirror::MirroredEntry) -> Option<ProtocolEvent> {
    Some(ProtocolEvent {
        id: entry.event_id,
        kind: entry.kind.clone(),
        issuer: global_id_from_str(&entry.issuer)?,
        subject: global_id_from_str(&entry.subject)?,
        payload: entry.payload.clone()?,
        timestamp: entry.event_timestamp,
        version: u32::try_from(entry.version).ok()?,
    })
}

/// `GlobalId` derives `Deserialize` as a transparent newtype over `String`,
/// so this is the same round trip a `ProtocolEvent`'s `issuer`/`subject`
/// field already goes through in every other JSON boundary in this crate —
/// there is no public raw-string constructor on `GlobalId` itself.
fn global_id_from_str(raw: &str) -> Option<GlobalId> {
    serde_json::from_value(serde_json::Value::String(raw.to_string())).ok()
}

/// For `identity.created` only: idempotently inserts the `identities` row
/// the event describes, from its own `identity_id` payload field — see the
/// call site's comment for why a remote-settlement node needs this at all.
/// Every other event kind is a no-op here; this is a narrow, known fix for
/// the one core-row dependency this ticket's live verification actually
/// hit, not a general "reconstruct all app-state from replay" mechanism —
/// other event kinds (`game.registered`, `guild.created`, ...) may have the
/// same class of gap against their own core tables and haven't been
/// verified; tracked as a follow-up rather than guessed at here.
async fn ensure_identity_row_exists(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    event: &ProtocolEvent,
) -> Result<(), MirrorWatcherError> {
    if event.kind != "identity.created" {
        return Ok(());
    }
    let Some(identity_id) = event
        .payload
        .get("identity_id")
        .and_then(|v| v.as_str())
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return Ok(());
    };
    sqlx::query("INSERT INTO identities (id) VALUES ($1) ON CONFLICT DO NOTHING")
        .bind(identity_id)
        .execute(&mut **tx)
        .await
        .map_err(|e| avalon_chain::SettlementError::Storage(e.to_string()))?;
    Ok(())
}

fn decode_proof_nodes(hex_nodes: &[String]) -> Result<Vec<[u8; 32]>, MirrorWatcherError> {
    hex_nodes
        .iter()
        .map(|hex_node| {
            let bytes = hex::decode(hex_node).map_err(|e| {
                MirrorWatcherError::Decode(format!("proof node not valid hex: {e}"))
            })?;
            <[u8; 32]>::try_from(bytes.as_slice())
                .map_err(|_| MirrorWatcherError::Decode("proof node was not 32 bytes".to_string()))
        })
        .collect()
}

/// Tries `GET /ledger/entries` against each of `candidates`, starting from
/// `*cursor` and wrapping around, returning the first success (and which
/// peer served it) — advancing `*cursor` past whichever candidate answered
/// so the next request in this backfill pass starts from a different one
/// rather than always retrying the same first candidate.
async fn fetch_entries_from_any(
    client: &reqwest::Client,
    candidates: &[String],
    cursor: &mut usize,
    since_seq: i64,
    limit: i64,
) -> Result<(Vec<LedgerEntryDto>, String), MirrorWatcherError> {
    for offset in 0..candidates.len() {
        let idx = (*cursor + offset) % candidates.len();
        let peer = &candidates[idx];
        match fetch_entries(client, peer, since_seq, limit).await {
            Ok(entries) => {
                *cursor = (idx + 1) % candidates.len();
                return Ok((entries, peer.clone()));
            }
            Err(err) => {
                eprintln!("mirror-watcher: {peer}: GET /ledger/entries failed, trying next candidate peer: {err}");
            }
        }
    }
    Err(MirrorWatcherError::AllPeersFailed)
}

/// Same round-robin-with-failover shape as [`fetch_entries_from_any`], for
/// `GET /ledger/proof/inclusion`.
async fn fetch_inclusion_proof_from_any(
    client: &reqwest::Client,
    candidates: &[String],
    cursor: &mut usize,
    seq: i64,
    tree_size: i64,
) -> Result<InclusionProofDto, MirrorWatcherError> {
    for offset in 0..candidates.len() {
        let idx = (*cursor + offset) % candidates.len();
        let peer = &candidates[idx];
        match fetch_inclusion_proof(client, peer, seq, tree_size).await {
            Ok(dto) => {
                *cursor = (idx + 1) % candidates.len();
                return Ok(dto);
            }
            Err(err) => {
                eprintln!("mirror-watcher: {peer}: GET /ledger/proof/inclusion failed, trying next candidate peer: {err}");
            }
        }
    }
    Err(MirrorWatcherError::AllPeersFailed)
}

async fn fetch_entries(
    client: &reqwest::Client,
    peer: &str,
    since_seq: i64,
    limit: i64,
) -> Result<Vec<LedgerEntryDto>, MirrorWatcherError> {
    let url = format!("{peer}/ledger/entries?since_seq={since_seq}&limit={limit}");
    let entries: Vec<LedgerEntryDto> = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(|e| MirrorWatcherError::Decode(e.to_string()))?;
    Ok(entries)
}

async fn fetch_inclusion_proof(
    client: &reqwest::Client,
    peer: &str,
    seq: i64,
    tree_size: i64,
) -> Result<InclusionProofDto, MirrorWatcherError> {
    let url = format!("{peer}/ledger/proof/inclusion?seq={seq}&tree_size={tree_size}");
    let dto: InclusionProofDto = client
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(|e| MirrorWatcherError::Decode(e.to_string()))?;
    Ok(dto)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Both cases live in one test (rather than two `#[test]` fns) because
    // `cargo test` runs tests in the same binary concurrently by default,
    // and both cases mutate the same process-wide `AVALON_MIRROR_PEERS` env
    // var — two separate tests racing on it would be flaky.
    #[test]
    fn from_env_reads_peers_and_poll_interval() {
        // SAFETY: test-only env mutation of vars no other test in this
        // binary touches.
        unsafe {
            std::env::remove_var("AVALON_MIRROR_PEERS");
        }
        assert!(MirrorWatcherConfig::from_env().is_none());

        unsafe {
            std::env::set_var(
                "AVALON_MIRROR_PEERS",
                "http://localhost:8081/, http://localhost:8082",
            );
            std::env::remove_var("AVALON_MIRROR_POLL_INTERVAL_SECS");
        }
        let config = MirrorWatcherConfig::from_env().expect("peers were set");
        assert_eq!(
            config.peers,
            vec!["http://localhost:8081", "http://localhost:8082"]
        );
        assert_eq!(config.poll_interval, Duration::from_secs(30));

        unsafe {
            std::env::remove_var("AVALON_MIRROR_PEERS");
        }
    }

    #[test]
    fn corroboration_picks_the_tree_head_the_most_peers_agree_on() {
        // Pure re-creation of `backfill_network`'s agreement-tallying
        // logic, without the network/DB calls around it — three peers
        // agree on one root_hash, one disagrees; the majority must win,
        // and the count must reflect three, not four.
        let sth_a = |root: &str| SignedTreeHead {
            tree_size: 10,
            root_hash: root.to_string(),
            network_id: "avalon-test".to_string(),
            signing_key_id: "k".to_string(),
            signature: "sig".to_string(),
            created_at: OffsetDateTime::UNIX_EPOCH,
        };
        let observations = vec![
            ("peer-a".to_string(), sth_a("aa")),
            ("peer-b".to_string(), sth_a("aa")),
            ("peer-c".to_string(), sth_a("aa")),
            ("peer-d".to_string(), sth_a("bb")),
        ];

        let mut agreement: HashMap<(i64, &str), Vec<&str>> = HashMap::new();
        for (peer, sth) in &observations {
            agreement
                .entry((sth.tree_size, sth.root_hash.as_str()))
                .or_default()
                .push(peer.as_str());
        }
        let ((_, winning_root), winners) = agreement
            .into_iter()
            .max_by_key(|(_, peers)| peers.len())
            .unwrap();

        assert_eq!(winning_root, "aa");
        assert_eq!(winners.len(), 3);
    }

    fn sample_mirrored_entry() -> mirror::MirroredEntry {
        mirror::MirroredEntry {
            source_url: "http://peer".to_string(),
            network_id: "avalon-test".to_string(),
            seq: 1,
            event_id: Uuid::new_v4(),
            kind: "identity.created".to_string(),
            issuer: "identity:11111111-1111-1111-1111-111111111111:self:created".to_string(),
            subject: "identity:11111111-1111-1111-1111-111111111111:self:created".to_string(),
            payload: Some(serde_json::json!({"display_name": "test"})),
            event_timestamp: OffsetDateTime::UNIX_EPOCH,
            version: 1,
            prev_hash: "aa".repeat(32),
            entry_hash: "bb".repeat(32),
            batch_id: Uuid::new_v4(),
            verified_tree_size: 1,
        }
    }

    #[test]
    fn decodes_a_verified_mirrored_entry_into_the_same_protocol_event_shape_the_outbox_builds() {
        let entry = sample_mirrored_entry();
        let event = protocol_event_from_mirrored(&entry).expect("well-formed entry decodes");

        assert_eq!(event.id, entry.event_id);
        assert_eq!(event.kind, entry.kind);
        assert_eq!(event.issuer.as_str(), entry.issuer);
        assert_eq!(event.subject.as_str(), entry.subject);
        assert_eq!(event.payload, entry.payload.unwrap());
        assert_eq!(event.timestamp, entry.event_timestamp);
        assert_eq!(event.version, entry.version as u32);
    }

    #[test]
    fn a_pruned_payload_decodes_to_none_rather_than_a_fabricated_event() {
        let mut entry = sample_mirrored_entry();
        entry.payload = None;

        assert!(protocol_event_from_mirrored(&entry).is_none());
    }
}
