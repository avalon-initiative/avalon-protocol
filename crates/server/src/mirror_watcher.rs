//! The mirror-watcher — verifies and stores STHs/entries
//! polled from configured peers, run as a background task inside
//! `avalon-server`. See `docs/projects/backend-server/architecture/nodes.md`'s "Today in the
//! repo" section for the multi-peer polling/backfill/equivocation-
//! detection design and why it lives in-process rather than as a CLI
//! daemon.
//!
//! **Two deliberate tiers.** Polling every configured peer on
//! [`MirrorWatcherConfig::poll_interval`] is the permissionless baseline —
//! mirroring this way needs zero registration and never will, since a
//! public transparency log must never gate reading on registering with
//! anyone. On top of that, this worker also registers this node's own
//! interest (via `crate::interest::InterestScope::for_network`, the exact
//! same DHT-backed registration built for guild-channel/conversation
//! routing) in every `network_id` it successfully verifies an STH for —
//! never in one it merely hopes to mirror, so a hostile or unpinned
//! `network_id` a peer might claim is never registered either. A
//! committing authority (`crate::mirror_push::notify_peers`) resolves that
//! registration and pushes a lightweight "go check" notification straight
//! to this node, which wakes [`run_worker`]'s loop early via
//! `AppState::mirror_wake` — see `crate::mirror_push`'s own module doc for
//! the delivery mechanics and why a push is never trusted directly. Poll's
//! role for a peer that's actually receiving pushes shrinks to "bound
//! worst-case staleness if a push is ever missed or this node's DHT
//! identity is disabled" — which is why [`MirrorWatcherConfig::from_env`]'s
//! default interval is now meaningfully longer than it used to be; see
//! its own doc comment.
//!
//! **`AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true` opts a node
//! into auto-mirroring every shard it discovers via peer-announce gossip
//! (`crate::nodes::ShardRegistry`), on top of whatever `AVALON_MIRROR_PEERS`
//! explicitly names.** [`discover_and_verify_shard_peers`] is the
//! deliberately separate verification path this needs: a
//! statically-configured `AVALON_MIRROR_PEERS` entry is verified against
//! this process's pinned `docs/trusted-networks.json` network trust
//! anchor (`fetch_and_verify_sth`, below) — the right check for mirroring
//! another *whole network*. A gossip-discovered shard is a different
//! shard *within this node's own network*, signed with an
//! integrator-registered `shard_settlement` key, which almost
//! never matches this network's root trust-anchor key — so a discovered
//! shard's STH is verified via `crate::cross_shard::resolve_shard_verify_keys_from_db`
//! instead, the exact same mechanism `crate::cross_shard`'s own
//! aggregation already uses. Only a shard whose STH verifies this way is
//! ever added to this tick's backfill targets; discovering a shard's
//! existence never implies trusting it. `AVALON_MIRROR_PEERS`/`AVALON_KNOWN_SHARDS` remain
//! valid, unchanged, narrower configuration — this is additive. Push
//! registration and discovered-shard auto-mirroring are independent
//! of each other and compose without special-casing: a discovered shard is
//! just another network-scoped mirror target as far as push/interest
//! registration is concerned.

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use avalon_chain::mirror::{self, ObservedSth, SELF_SIGNED_SOURCE};
use avalon_chain::{merkle, PostgresSettlementProvider};
use avalon_indexer::postgres::PostgresIndexer;
use avalon_protocol::cosigned_sth::CosignedTreeHead;
use avalon_protocol::events::ProtocolEvent;
use avalon_protocol::ids::GlobalId;
use avalon_protocol::sth::SignedTreeHead;
use ed25519_dalek::VerifyingKey;
use serde::Deserialize;
use sqlx::Acquire;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use crate::cosign_gather;
use crate::cosign_verify::{self, WitnessCosignatureDto};
use crate::known_list::KnownListHandle;
use crate::nodes::HeadGossipTracker;
use crate::witness_cosign::{self, WitnessCosignConfig};

/// How many entries to request per bulk-entries page while backfilling.
const BACKFILL_PAGE_SIZE: i64 = 200;

/// Issue #573: `AVALON_MIRROR_PEERS` entries can now be either a bare URL
/// (implicitly the `"core"` shard, preserving every pre-#573 config
/// byte-for-byte) or `shard_id=url` — the same bare-vs-keyed convention
/// `AVALON_SETTLEMENT_REMOTE_URLS`/`AVALON_KNOWN_SHARDS` already use.
/// Shared by [`MirrorWatcherConfig::from_env`] (which only needs the bare
/// URL list to poll) and `crate::settlement::ShardMirrorSources` (which
/// needs the shard_id mapping too, to scope reads correctly — see its own
/// doc comment for why this matters).
pub(crate) fn parse_mirror_peers(raw: &str) -> Vec<(String, String)> {
    raw.split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            if entry.is_empty() {
                return None;
            }
            let (shard_id, url) = match entry.split_once('=') {
                Some((shard_id, url)) => (shard_id.trim().to_string(), url.trim()),
                None => ("core".to_string(), entry),
            };
            let url = url.trim_end_matches('/').to_string();
            if url.is_empty() {
                None
            } else {
                Some((shard_id, url))
            }
        })
        .collect()
}

pub struct MirrorWatcherConfig {
    /// `(shard_id, base_url)` pairs to watch, e.g.
    /// `("core", "http://localhost:8081")` — no trailing slash on the URL
    /// (stripped in [`Self::from_env`] if present). More than one is a
    /// normal, supported configuration — see module docs. Issue #604:
    /// `shard_id` is carried alongside each URL (not discarded) precisely
    /// because polling *does* need to know which shard it's asking a peer
    /// about — a peer serving more than one shard answers a bare
    /// `/ledger/sth/latest` with whichever it treats as its own default,
    /// not necessarily the one this entry means (#573's documented
    /// footgun).
    pub peers: Vec<(String, String)>,
    pub poll_interval: Duration,
    /// Every `shard_id` explicitly named in `AVALON_MIRROR_PEERS`
    /// (`"core"` for a bare, un-prefixed entry) — issue #599's
    /// auto-discovery pass skips these, since an operator who explicitly
    /// configured a shard already gets it mirrored via `peers` above and
    /// verified via the network-trust-anchor path; auto-discovery only
    /// ever adds a shard this config doesn't already name.
    pub known_shard_ids: BTreeSet<String>,
    /// `AVALON_MIRROR_ALL_DISCOVERED_SHARDS` — see this
    /// module's own doc comment. Opt-in, independent of whether `peers`
    /// above is empty: a node can auto-mirror discovered shards with zero
    /// explicit `AVALON_MIRROR_PEERS` configuration at all.
    pub auto_mirror_discovered: bool,
}

impl MirrorWatcherConfig {
    /// `Some` when either `AVALON_MIRROR_PEERS` names at least one peer,
    /// or `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true` — either
    /// alone is enough for this worker to have something to do; `None`
    /// (neither set) is the signal `main.rs` uses to skip spawning the
    /// watcher entirely, the same "only spawn if configured" pattern
    /// `retention::RetentionConfig::should_prune` already uses.
    ///
    /// Poll interval defaults to 120s (raised from 30s),
    /// overridable via `AVALON_MIRROR_POLL_INTERVAL_SECS` — an STH is a
    /// few hundred bytes of JSON, so bandwidth was never the constraint;
    /// 30s was originally chosen as a sensible cadence for a purely
    /// poll-driven design. Now that a registered peer additionally gets a
    /// low-latency push the moment a new STH actually exists (see this
    /// module's own doc comment), polling's remaining job for that peer is
    /// only to bound worst-case staleness if a push is ever missed — which
    /// doesn't need a 30s cadence. 120s keeps that bound comfortably tight
    /// (a missed push costs at most two minutes of extra staleness, not
    /// thirty seconds) while cutting this worker's at-idle HTTP overhead
    /// by 4x for every deployment, registered or not — see
    /// `docs/projects/backend-server/architecture/nodes.md`'s mirror-sync section for the same
    /// reasoning written up for operators.
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("AVALON_MIRROR_PEERS")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let peers = raw.as_deref().map(parse_mirror_peers).unwrap_or_default();
        let known_shard_ids: BTreeSet<String> = peers
            .iter()
            .map(|(shard_id, _url)| shard_id.clone())
            .collect();

        let auto_mirror_discovered = std::env::var("AVALON_MIRROR_ALL_DISCOVERED_SHARDS")
            .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
            .unwrap_or(false);

        if peers.is_empty() && !auto_mirror_discovered {
            return None;
        }

        let poll_interval = std::env::var("AVALON_MIRROR_POLL_INTERVAL_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS));
        Some(Self {
            peers,
            poll_interval,
            known_shard_ids,
            auto_mirror_discovered,
        })
    }
}

/// See [`MirrorWatcherConfig::from_env`]'s own doc comment for why this
/// changed from 30 to 120.
const DEFAULT_POLL_INTERVAL_SECS: u64 = 120;

/// Verifies (never trusts by discovery alone) every
/// gossip-discovered shard this node hasn't already explicitly
/// configured, when `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true`. See this
/// module's own doc comment for why this uses a different key-resolution
/// path (`crate::cross_shard::resolve_shard_verify_keys_from_db`) than
/// [`fetch_and_verify_sth`] below — the acceptance rule itself (majority
/// cosignature against `known_list`, degenerating to plain
/// author-signature verification at 0 or 1) is the same.
/// Returns `(shard_id, url, CosignedTreeHead, author_verify_key)` for every
/// discovered shard whose STH verified — a shard that's unreachable, has no
/// registered key yet, or fails verification is simply left out this tick
/// (retried again next tick, never trusted on spec alone). The matched
/// author key is returned alongside the head for the same reason
/// [`fetch_and_verify_sth`] returns one: a caller cross-checking two
/// accepted heads for equivocation needs the exact key both verified
/// against.
async fn discover_and_verify_shard_peers(
    client: &reqwest::Client,
    pool: &PgPool,
    network_id: &str,
    shard_registry: &crate::nodes::ShardRegistry,
    own_base_url: Option<&str>,
    already_configured: &BTreeSet<String>,
) -> Vec<(String, String, CosignedTreeHead, VerifyingKey)> {
    let mut verified = Vec::new();
    for shard_id in shard_registry.known_shard_ids() {
        if already_configured.contains(&shard_id) {
            continue;
        }
        let Some(url) = shard_registry.best_url(&shard_id) else {
            continue;
        };
        if Some(url.as_str()) == own_base_url {
            continue;
        }

        let db_keys =
            crate::cross_shard::resolve_shard_verify_keys_from_db(pool, network_id, &shard_id)
                .await;
        if db_keys.is_empty() {
            tracing::info!(
                shard_id,
                url = %url,
                "mirror-watcher: discovered shard has no #543-registered shard_settlement key \
                 resolved yet — not auto-mirroring until one is",
            );
            continue;
        }

        match fetch_latest_sth(client, &url, Some(&shard_id)).await {
            Ok((dto, _peer_protocol_version)) => {
                let head: CosignedTreeHead = dto.into();
                let now = OffsetDateTime::now_utc();
                match cosign_verify::verify_cosigned_against_any_key(
                    db_keys.iter().copied(),
                    &head,
                    &[],
                    now,
                ) {
                    Some(matched_key) => {
                        tracing::info!(
                            event = "auto_mirror_discovered_shard",
                            shard_id,
                            url = %url,
                            "auto-mirroring a newly discovered shard whose STH verified against \
                             a #543-registered shard_settlement key",
                        );
                        verified.push((shard_id, url, head, matched_key));
                    }
                    None => {
                        tracing::warn!(
                            shard_id,
                            url = %url,
                            "mirror-watcher: discovered shard's STH failed verification against \
                             every #543-registered key (or lacked majority cosignature) — not \
                             auto-mirroring",
                        );
                    }
                }
            }
            Err(err) => tracing::warn!(
                shard_id,
                url = %url,
                error = %err,
                "mirror-watcher: failed to fetch STH for a discovered shard",
            ),
        }
    }
    verified
}

/// Spawned once at startup (see `main.rs`) when [`MirrorWatcherConfig::from_env`]
/// returns `Some`. Never returns. See module docs for the two-phase
/// per-tick shape: every peer is polled for its STH independently first
/// (phase 1), then peers are grouped by `network_id` and backfilled
/// together (phase 2) — one unreachable/misbehaving peer never stops the
/// others from being watched or from covering for it during backfill.
///
/// Bundles [`run_worker`]'s dependencies beyond the core
/// pool/chain/indexer/config quartet — `interest`/`own_base_url`
/// register this node's own interest in every `network_id` phase 1
/// actually verifies an STH for (harmless, no-op bookkeeping if
/// `AVALON_DHT_ENABLED` is unset, see `crate::interest`'s own module doc
/// comment on that); `wake` is woken by `crate::mirror_push::notify`'s
/// handler on an incoming push notification, short-circuiting the rest of
/// the current poll interval; `shard_registry`/`own_base_url` also feed
/// [`discover_and_verify_shard_peers`]; `own_shard_id`
/// is this node's own locally-authored shard, needed so
/// [`check_equivocation`] only folds this node's own signed history into a
/// comparison for observations of *that* shard. Grouped into one struct
/// purely to keep [`run_worker`]'s own signature under clippy's
/// too-many-arguments threshold — no shared lifecycle beyond that.
pub struct MirrorWatcherHandles {
    pub interest: crate::interest::InterestRegistry,
    pub shard_registry: crate::nodes::ShardRegistry,
    pub own_base_url: Option<String>,
    pub wake: std::sync::Arc<tokio::sync::Notify>,
    pub own_shard_id: String,
    /// This node's own witness known list, read live on every
    /// verification — never a snapshot taken once at startup, so admitting
    /// or dropping a witness takes effect on this worker's very next poll
    /// tick, no restart needed. See `crate::cosign_verify`'s own module doc
    /// comment for the identity-bridging caveat.
    pub known_list: KnownListHandle,
    /// Consulted before every cosigning decision: a shard with a confirmed
    /// equivocation on record gets no further cosignatures from this node.
    pub head_gossip: HeadGossipTracker,
    /// `None` when this node has opted out of cosigning or has no witness
    /// signing key — see [`WitnessCosignConfig::from_env`].
    pub witness: Option<WitnessCosignConfig>,
    /// Peer table used to resolve a known-list witness's base URL when
    /// gathering cosignatures.
    pub peers: crate::nodes::PeerTable,
    /// Trust anchors a peer's claimed `network_id` is resolved against.
    pub trust_anchors: Vec<avalon_protocol::network_trust::TrustAnchorEntry>,
}

/// What `record_verified_head` needs to decide whether an author-verified
/// head has reached majority: this tick's known list and how to reach each
/// of its witnesses.
struct MajorityContext<'a> {
    known_list: &'a [(String, VerifyingKey)],
    sources: &'a [cosign_gather::WitnessSource],
    policy: crate::outbound_policy::OutboundPolicy,
}

type HeldHeads = std::collections::HashSet<(String, i64, String)>;
const MAX_HELD_LOG_ENTRIES: usize = 1024;

/// Spawned once at startup (see `main.rs`) when [`MirrorWatcherConfig::from_env`]
/// returns `Some`. Never returns. See module docs for the two-phase
/// per-tick shape: every peer is polled for its STH independently first
/// (phase 1), then peers are grouped by `network_id`/`shard_id` and
/// backfilled together (phase 2) — one unreachable/misbehaving peer never
/// stops the others from being watched or from covering for it during
/// backfill. See [`MirrorWatcherHandles`]'s own doc comment for what
/// `handles` carries and why.
pub async fn run_worker(
    pool: PgPool,
    chain: PostgresSettlementProvider,
    indexer: PostgresIndexer,
    config: MirrorWatcherConfig,
    handles: MirrorWatcherHandles,
) {
    let MirrorWatcherHandles {
        interest,
        shard_registry,
        own_base_url,
        wake,
        own_shard_id,
        known_list,
        head_gossip,
        witness,
        peers,
        trust_anchors,
    } = handles;
    let mut held_logged = HeldHeads::new();
    let policy = crate::outbound_policy::OutboundPolicy::from_env();
    let client = crate::outbound_policy::peer_client();

    if config.peers.is_empty() {
        tracing::info!(
            "mirror-watcher: no statically-configured peers (AVALON_MIRROR_PEERS unset) — \
             auto_mirror_discovered={}",
            config.auto_mirror_discovered
        );
    } else {
        let peers_display: Vec<String> = config
            .peers
            .iter()
            .map(|(shard_id, url)| format!("{shard_id}={url}"))
            .collect();
        tracing::info!(
            "mirror-watcher: watching {} peer(s) every {:?}: {} (auto_mirror_discovered={})",
            config.peers.len(),
            config.poll_interval,
            peers_display.join(", "),
            config.auto_mirror_discovered
        );
    }
    if own_base_url.is_none() {
        tracing::info!(
            "mirror-watcher: AVALON_NODE_URL is unset — this node can still receive push \
             notifications targeted at whatever interest it registers, but registered peers \
             pushing to it will only reach it once it has a reachable base URL to advertise"
        );
    }

    // Issue #596: held for the life of this worker, one guard per
    // network_id this tick's phase 1 has ever verified an STH for — never
    // dropped, since this node keeps mirroring that network for as long as
    // it's configured to. Registering here (only once an STH has actually
    // verified, never on a bare, unverified peer URL) is what keeps a
    // hostile or unpinned network_id claim from ever reaching the DHT.
    let mut network_interest: HashMap<String, crate::interest::InterestGuard> = HashMap::new();

    loop {
        // Read live at the top of every tick — never a snapshot taken once
        // at startup — so an admitted or dropped known-list witness is
        // reflected starting this very tick, no restart needed.
        let known_list_pairs = cosign_verify::known_list_verifying_keys(&known_list);
        let witness_sources = cosign_gather::witness_sources(&known_list_pairs, &peers.list_all());
        let majority = MajorityContext {
            known_list: &known_list_pairs,
            sources: &witness_sources,
            policy,
        };

        // Phase 1: poll every peer independently for its latest STH,
        // verify + store + equivocation-check each one. Peers that
        // succeed are grouped by (network_id, shard_id), since that's
        // what phase 2 backfills over and two different
        // shards under the same network_id must never be corroborated or
        // backfilled together, even if they happen to report the same
        // tree_size.
        let mut verified_by_shard: HashMap<(String, String), Vec<(String, CosignedTreeHead)>> =
            HashMap::new();
        for (shard_id, peer) in &config.peers {
            match fetch_and_verify_sth(&client, &trust_anchors, peer, shard_id).await {
                Ok((head, author_key)) => {
                    record_verified_head(
                        &pool,
                        &chain,
                        &client,
                        witness.as_ref(),
                        &head_gossip,
                        Some((&interest, &mut network_interest)),
                        &mut verified_by_shard,
                        &own_shard_id,
                        shard_id,
                        peer,
                        head,
                        author_key,
                        &majority,
                        &mut held_logged,
                    )
                    .await;
                }
                Err(err) => tracing::error!("mirror-watcher: {peer}: {err}"),
            }
        }

        if config.auto_mirror_discovered {
            let discovered = discover_and_verify_shard_peers(
                &client,
                &pool,
                chain.network_id(),
                &shard_registry,
                own_base_url.as_deref(),
                &config.known_shard_ids,
            )
            .await;
            for (shard_id, peer, head, author_key) in discovered {
                // Unlike the statically-configured loop above,
                // a purely gossip-discovered shard never registers
                // push-based mirror-sync interest — discovering a shard's
                // existence is not the same as this node committing to
                // keep watching it the way an explicit
                // `AVALON_MIRROR_PEERS` entry does.
                record_verified_head(
                    &pool,
                    &chain,
                    &client,
                    witness.as_ref(),
                    &head_gossip,
                    None,
                    &mut verified_by_shard,
                    &own_shard_id,
                    &shard_id,
                    &peer,
                    head,
                    author_key,
                    &majority,
                    &mut held_logged,
                )
                .await;
            }
        }

        // Phase 2: for each (network, shard) at least one peer reported
        // this tick, pick a corroborated tree head and backfill against
        // every peer that agreed on it.
        for ((network_id, shard_id), heads) in &verified_by_shard {
            let observations: Vec<(String, SignedTreeHead)> = heads
                .iter()
                .map(|(peer, head)| (peer.clone(), head.sth.clone()))
                .collect();
            if let Err(err) = backfill_network(
                &client,
                &pool,
                &indexer,
                network_id,
                shard_id,
                &observations,
            )
            .await
            {
                tracing::error!("mirror-watcher: {network_id}/{shard_id}: {err}");
            }
        }

        // Issue #596: wait for whichever comes first — the normal poll
        // interval, or a push notification waking this loop early. Either
        // way the next iteration runs the exact same verify/corroborate/
        // backfill pipeline above; a push only ever changes *when* that
        // runs, never what it trusts.
        tokio::select! {
            _ = tokio::time::sleep(config.poll_interval) => {}
            _ = wake.notified() => {
                tracing::info!(
                    "mirror-watcher: woke early due to a push notification, re-polling now"
                );
            }
        }
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
    /// Issue #513/#515: distinguishable from [`Self::InvalidSignature`] on
    /// purpose — there's no key to even attempt verification against,
    /// because this `network_id` has no entry in
    /// `docs/trusted-networks.json`. A node only ever mirrors networks it
    /// has an explicit, reviewed trust anchor for; an unpinned
    /// `network_id` (typo, unrelated network, or a peer just claiming
    /// whatever it wants) is refused the same way an invalid signature is,
    /// never silently stored.
    #[error("peer claims network_id {0:?}, which has no pinned trust anchor in docs/trusted-networks.json — refusing to mirror it")]
    UnpinnedNetwork(String),
    #[error("inclusion proof failed verification for seq={seq} against tree_size={tree_size}")]
    InvalidInclusionProof { seq: i64, tree_size: i64 },
    #[error("peer's inclusion-proof root_hash did not match the already-verified STH root_hash")]
    RootHashMismatch,
    #[error("every configured peer for this network failed this request")]
    AllPeersFailed,
    #[error("storage error: {0}")]
    Storage(#[from] avalon_chain::SettlementError),
    /// Issue #368: distinguishable from [`Self::Decode`] on purpose — this
    /// peer's response decoded just fine, it's the reported version that
    /// doesn't clear this node's floor (or is empty/unparseable). Never a
    /// panic, and reversible: the peer's next STH is re-checked fresh on
    /// this node's next poll tick, so an upgrade is picked up automatically.
    #[error("peer reported protocol_version {peer_version:?}, below this node's effective floor {floor}")]
    IncompatiblePeerVersion { peer_version: String, floor: String },
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
    /// Issue #368: additive (`#[serde(default)]`) so an older peer's
    /// response without this field still decodes fine — it's reported as
    /// an empty string, which `crate::version::is_supported` always
    /// treats as unsupported (never given the benefit of the doubt), same
    /// as a genuinely malformed version string.
    #[serde(default)]
    protocol_version: String,
    /// Additive (`#[serde(default)]`) so an older peer's
    /// response without this field still decodes fine — an empty list,
    /// which `cosigned_sth::verify_cosigned_tree_head` treats exactly like
    /// a head this node simply hasn't collected any cosignatures for yet.
    #[serde(default)]
    cosignatures: Vec<WitnessCosignatureDto>,
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

impl From<SignedTreeHeadDto> for CosignedTreeHead {
    fn from(dto: SignedTreeHeadDto) -> Self {
        let cosignature_dtos = dto.cosignatures.clone();
        let sth: SignedTreeHead = dto.into();
        let cosignatures = cosignature_dtos
            .iter()
            .map(|c| c.to_witness_cosignature(&sth))
            .collect();
        CosignedTreeHead { sth, cosignatures }
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

/// Pure check, split out for direct unit testing without a live peer:
/// `Err` (never a panic) if `peer_protocol_version` doesn't clear this
/// node's effective floor — logged as a clear, distinct "incompatible"
/// event, whether the reported version is empty (an older peer, or a
/// decode that fell back to `SignedTreeHeadDto`'s `#[serde(default)]`),
/// unparseable, or simply below the floor.
fn check_peer_version(peer: &str, peer_protocol_version: &str) -> Result<(), MirrorWatcherError> {
    if crate::version::is_supported(peer_protocol_version) {
        return Ok(());
    }
    let floor = crate::version::effective_min_peer_version().to_string();
    tracing::error!(
        event = "incompatible_peer_version",
        peer = %peer,
        peer_version = %peer_protocol_version,
        floor = %floor,
        "peer's reported protocol_version is below this node's effective floor — not \
         trusting this STH; this is a compatibility/availability signal only, never a \
         security check, and is self-correcting once the peer upgrades",
    );
    Err(MirrorWatcherError::IncompatiblePeerVersion {
        peer_version: peer_protocol_version.to_string(),
        floor,
    })
}

/// Fetches `peer`'s latest STH for `shard_id` and verifies its author
/// signature — the one step every peer goes through in phase 1. Majority
/// cosignature is decided afterwards by `record_verified_head`, so cosigning
/// never depends on it. Issue #604: `shard_id` is sent as an explicit `?shard_id=` query
/// param (#573's documented footgun) — a peer serving more than one shard
/// (mirroring one, authoring another) answers a bare request with whichever
/// it treats as its own default, not necessarily the one this config entry
/// means.
///
/// Returns the accepted head alongside the author key it verified against
/// — a caller cross-checking two accepted heads for equivocation
/// (`run_worker`) needs the same key both were checked against.
async fn fetch_and_verify_sth(
    client: &reqwest::Client,
    anchors: &[avalon_protocol::network_trust::TrustAnchorEntry],
    peer: &str,
    shard_id: &str,
) -> Result<(CosignedTreeHead, VerifyingKey), MirrorWatcherError> {
    let (dto, peer_protocol_version) = fetch_latest_sth(client, peer, Some(shard_id)).await?;
    check_peer_version(peer, &peer_protocol_version)?;

    let Some(verify_key) = verify_key_for_network(anchors, &dto.network_id) else {
        tracing::error!(
            event = "mirror_peer_network_unpinned",
            peer = %peer,
            network_id = %dto.network_id,
            "peer claims a network_id with no pinned trust anchor — not mirroring",
        );
        return Err(MirrorWatcherError::UnpinnedNetwork(dto.network_id));
    };

    let head: CosignedTreeHead = dto.into();
    let now = OffsetDateTime::now_utc();
    let Some(matched_key) =
        cosign_verify::verify_cosigned_against_any_key([verify_key], &head, &[], now)
    else {
        tracing::error!(
            event = "sth_verification_failed",
            peer = %peer,
            network_id = %head.sth.network_id,
            tree_size = head.sth.tree_size,
            "STH failed verification: the author signature is invalid — not storing, not trusting",
        );
        return Err(MirrorWatcherError::InvalidSignature);
    };
    Ok((head, matched_key))
}

/// Resolves the verify key for whatever `network_id` a peer's STH actually
/// claims, rather than one process-wide key — the same
/// per-network trust-anchor lookup `avalon_protocol::network_trust::evaluate_network_trust`
/// uses client-side, applied here so a node mirroring peers across
/// more than one legitimate, pinned network verifies each against its own
/// correct key. `None` for a `network_id` with no entry in
/// `docs/trusted-networks.json` at all — the caller treats that as a hard
/// refusal, never a fallback to some other key. Pure, for
/// direct unit testing (same split-out pattern `nodes::resolve_bootstrap_peers`
/// already uses in this repo).
fn verify_key_for_network(
    anchors: &[avalon_protocol::network_trust::TrustAnchorEntry],
    network_id: &str,
) -> Option<VerifyingKey> {
    let entry = anchors.iter().find(|a| a.network_id == network_id)?;
    let bytes = hex::decode(&entry.verify_key).ok()?;
    let key_array: [u8; 32] = bytes.as_slice().try_into().ok()?;
    VerifyingKey::from_bytes(&key_array).ok()
}

/// Returns the decoded STH alongside the peer's separately-reported
/// `protocol_version` — kept apart from [`SignedTreeHead`] itself (see
/// `SignedTreeHeadDto`'s own doc comment): the version is never part of
/// the signed payload.
///
/// `shard_id`, when given, is sent as an explicit `?shard_id=` query
/// param — a footgun restated here since [`discover_and_verify_shard_peers`]
/// found it live: a peer serving more than one shard (mirroring one,
/// authoring another, same as `avalon-peer` in this project's own sandbox
/// topology) answers a bare `/ledger/sth/latest` with whichever shard
/// *it* treats as its own default, not necessarily the one being asked
/// about — silently fetching and then failing to verify the wrong
/// shard's STH entirely. `None` preserves the original bare-request
/// behavior for callers that already know they're talking to a
/// single-shard peer (or are deliberately asking for that peer's own
/// default).
async fn fetch_latest_sth(
    client: &reqwest::Client,
    peer: &str,
    shard_id: Option<&str>,
) -> Result<(SignedTreeHeadDto, String), MirrorWatcherError> {
    let url = format!("{peer}/ledger/sth/latest");
    let mut request = client.get(&url);
    if let Some(shard_id) = shard_id {
        request = request.query(&[("shard_id", shard_id)]);
    }
    let dto: SignedTreeHeadDto = send_with_rate_limit_retry(request)
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(|e| MirrorWatcherError::Decode(e.to_string()))?;
    let protocol_version = dto.protocol_version.clone();
    Ok((dto, protocol_version))
}

/// Sends `request`, retrying with backoff on HTTP 429 — live
/// testing surfaced this codebase's own rate limiter as a real
/// concern for backfill, not just a hypothetical: backfilling a real,
/// busy history's worth of individual `/ledger/entries`/`/ledger/proof/inclusion`
/// requests can legitimately exceed `AVALON_RATE_LIMIT_PER_MINUTE`
/// mid-backfill, and treating that as an ordinary peer failure (this
/// tick's backfill just aborts, retried whole-hog next poll interval)
/// makes backfilling any sufficiently large history painfully slow at
/// best. Respects a `Retry-After` header when the rate limiter sends one
/// (`GovernorLayer` does), else falls back to a short fixed
/// backoff. Bounded to a handful of attempts so a persistently
/// misbehaving/adversarial peer still surfaces as a real failure rather
/// than retrying forever — the existing per-tick retry (next poll
/// interval) is still the ultimate backstop.
async fn send_with_rate_limit_retry(
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, reqwest::Error> {
    const MAX_RATE_LIMIT_RETRIES: u32 = 5;
    const DEFAULT_BACKOFF: std::time::Duration = std::time::Duration::from_millis(500);

    let mut attempt = 0u32;
    loop {
        let this_attempt = request
            .try_clone()
            .expect("GET requests built here never stream a body, so cloning always succeeds");
        let response = this_attempt.send().await?;
        if response.status() != reqwest::StatusCode::TOO_MANY_REQUESTS
            || attempt >= MAX_RATE_LIMIT_RETRIES
        {
            return Ok(response);
        }
        // Issue #604: a `Retry-After: 0` (this rate limiter's GCRA
        // algorithm regenerates tokens continuously, so "0 whole seconds"
        // is a real, common answer, not "retry immediately") still needs
        // a real, non-zero floor — retrying in an actual tight loop just
        // re-hits the same still-exhausted token bucket.
        let wait = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(std::time::Duration::from_secs)
            .unwrap_or(DEFAULT_BACKOFF)
            .max(DEFAULT_BACKOFF);
        attempt += 1;
        tracing::warn!(
            attempt,
            wait_secs = wait.as_secs_f64(),
            "mirror-watcher: rate-limited (429) fetching from a peer, backing off and retrying"
        );
        tokio::time::sleep(wait).await;
    }
}

/// Durably stores every cosignature attached to `head` that
/// this node can itself vouch for — signature-valid against a key its own
/// known list currently recognizes. Cosignatures from a witness this node
/// doesn't (yet, or ever) recognize are never stored: an unbounded stream
/// of junk cosignatures from an adversarial peer would otherwise grow this
/// table forever for no benefit, and this node has no way to tell a
/// legitimate-but-unrecognized witness from a fabricated one anyway.
/// Best-effort — a storage failure here never fails verification of the
/// head itself, which already succeeded before this is ever called; a
/// cosignature this node fails to persist is simply not available to
/// re-serve to further mirrors, not a correctness problem for this node's
/// own view.
async fn store_valid_cosignatures(
    chain: &PostgresSettlementProvider,
    shard_id: &str,
    head: &CosignedTreeHead,
    known_list: &[(String, VerifyingKey)],
) {
    for cosig in &head.cosignatures {
        let Some((_, verifying_key)) = known_list
            .iter()
            .find(|(witness_key_id, _)| *witness_key_id == cosig.witness_key_id)
        else {
            continue;
        };
        if !avalon_protocol::witness::verify_witness_cosignature(verifying_key, cosig) {
            continue;
        }
        if let Err(err) = chain.store_witness_cosignature(shard_id, cosig).await {
            tracing::warn!(
                witness_key_id = %cosig.witness_key_id,
                shard_id,
                error = %err,
                "mirror-watcher: failed to durably store a valid witness cosignature",
            );
        }
    }
}

/// The shared bookkeeping every head accepted by
/// [`fetch_and_verify_sth`] or [`discover_and_verify_shard_peers`] goes
/// through — recording the raw observation (feeding the original
/// source-based equivocation detection, unchanged by cosigning), durably
/// storing whatever cosignatures this node can itself vouch for, checking
/// for a genuine *witness*-cosigned equivocation against every other head
/// already accepted this tick for the same network/shard/tree_size, and
/// finally recording the head for phase 2's corroboration/backfill
/// decision. `interest` is `Some` only for the statically-configured peer
/// loop — see that call site's own comment for why a purely
/// gossip-discovered shard never registers push interest.
#[allow(clippy::too_many_arguments)]
async fn record_verified_head(
    pool: &PgPool,
    chain: &PostgresSettlementProvider,
    client: &reqwest::Client,
    witness: Option<&WitnessCosignConfig>,
    head_gossip: &HeadGossipTracker,
    interest: Option<(
        &crate::interest::InterestRegistry,
        &mut HashMap<String, crate::interest::InterestGuard>,
    )>,
    verified_by_shard: &mut HashMap<(String, String), Vec<(String, CosignedTreeHead)>>,
    own_shard_id: &str,
    shard_id: &str,
    peer: &str,
    head: CosignedTreeHead,
    author_key: VerifyingKey,
    majority: &MajorityContext<'_>,
    held_logged: &mut HeldHeads,
) {
    let observed = ObservedSth::from_sth(peer, shard_id, &head.sth, OffsetDateTime::now_utc());
    match mirror::insert_observation(pool, &observed).await {
        Ok(is_new) => {
            if is_new {
                if let Err(err) = check_equivocation(pool, chain, own_shard_id, &observed).await {
                    tracing::error!("mirror-watcher: {peer}: {err}");
                }
            }
        }
        Err(err) => {
            tracing::error!("mirror-watcher: {peer}: {err}");
            return;
        }
    }

    // Cosigning depends only on the author signature (checked by the caller)
    // plus the consistency and double-cosign guards, never on majority.
    witness_cosign::decide_and_cosign(
        chain,
        pool,
        client,
        witness,
        head_gossip,
        peer,
        shard_id,
        &head,
    )
    .await;

    let held_key = (
        shard_id.to_string(),
        head.sth.tree_size,
        head.sth.root_hash.clone(),
    );
    let head = match cosign_gather::resolve_majority(
        majority.policy,
        &author_key,
        head,
        majority.known_list,
        majority.sources,
        shard_id,
    )
    .await
    {
        cosign_gather::HeadVerdict::Trusted(head) => head,
        cosign_gather::HeadVerdict::Held => {
            if held_logged.len() >= MAX_HELD_LOG_ENTRIES {
                held_logged.clear();
            }
            if held_logged.insert(held_key.clone()) {
                tracing::info!(
                    peer = %peer,
                    shard_id,
                    tree_size = held_key.1,
                    "mirror-watcher: head's author signature is valid but cosignatures do not \
                     reach majority yet — holding it, retrying next tick",
                );
            }
            return;
        }
    };
    let known_list = majority.known_list;

    store_valid_cosignatures(chain, shard_id, &head, known_list).await;

    // A genuinely provable *witness* equivocation — only meaningful once
    // real cosigning was actually required to accept a head at all
    // (`known_list.len() > 1`; at or below that, both heads were only ever
    // checked against the bare author signature per
    // `verify_cosigned_tree_head`'s own degenerate case, which the
    // source-based `check_equivocation` above already covers as an author
    // equivocation, not a witness one).
    if known_list.len() > 1 {
        if let Some(existing_heads) =
            verified_by_shard.get(&(head.sth.network_id.clone(), shard_id.to_string()))
        {
            let now = OffsetDateTime::now_utc();
            let freshness_cutoff = now - cosign_verify::COSIGNATURE_FRESHNESS_WINDOW;
            for (_, other_head) in existing_heads {
                let equivocators = avalon_protocol::cosigned_sth::find_equivocating_witnesses(
                    &author_key,
                    known_list,
                    freshness_cutoff,
                    now,
                    other_head,
                    &head,
                );
                if !equivocators.is_empty() {
                    let evidence = mirror::WitnessEquivocationEvidence {
                        kind: mirror::EquivocationEvidenceKind::Witness,
                        network_id: head.sth.network_id.clone(),
                        shard_id: shard_id.to_string(),
                        tree_size: head.sth.tree_size,
                        head_a: other_head.clone(),
                        head_b: head.clone(),
                        equivocating_witness_key_ids: equivocators,
                        detected_at: None,
                    };
                    if let Err(err) =
                        mirror::record_witness_equivocation_evidence(pool, &evidence).await
                    {
                        tracing::error!(
                            "mirror-watcher: {peer}: failed to durably record witness \
                             equivocation evidence: {err}"
                        );
                    }
                }
            }
        }
    }

    if let Some((interest, network_interest)) = interest {
        network_interest
            .entry(head.sth.network_id.clone())
            .or_insert_with(|| {
                tracing::info!(
                    network_id = %head.sth.network_id,
                    "mirror-watcher: registering interest for push-based mirror sync (issue #596)"
                );
                interest.register(crate::interest::InterestScope::for_network(
                    &head.sth.network_id,
                ))
            });
    }
    verified_by_shard
        .entry((head.sth.network_id.clone(), shard_id.to_string()))
        .or_default()
        .push((peer.to_string(), head));
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
    own_shard_id: &str,
    observed: &ObservedSth,
) -> Result<(), MirrorWatcherError> {
    let mut existing = mirror::observations_at(
        pool,
        &observed.network_id,
        &observed.shard_id,
        observed.tree_size,
    )
    .await?;

    // Fold in this node's own signed history, if it has one at this exact
    // tree_size — a node that is both an authority and a mirror must catch
    // itself disagreeing with what it broadcasts, not just catch two peers
    // disagreeing with each other. Issue #604: only when `observed` is
    // actually about this node's own authored shard — folding it in for a
    // *different* shard this node happens to also be mirroring would
    // compare across two unrelated logs, exactly the bug class this
    // ticket fixes.
    if chain.network_id() == observed.network_id && own_shard_id == observed.shard_id {
        if let Some(own) = chain.signed_tree_head_at(observed.tree_size).await? {
            existing.push(ObservedSth::from_sth(
                SELF_SIGNED_SOURCE,
                own_shard_id,
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
    shard_id: &str,
    observations: &[(String, SignedTreeHead)],
) -> Result<(), MirrorWatcherError> {
    let equivocations = mirror::unresolved_equivocations(pool, network_id, shard_id).await?;
    if !equivocations.is_empty() {
        tracing::error!(
            event = "equivocation_backfill_blocked",
            network_id = %network_id,
            shard_id = %shard_id,
            unresolved_findings = equivocations.len(),
            "refusing to backfill — unresolved equivocation finding(s) recorded for this shard; needs human investigation before further backfill can be trusted",
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
        tracing::info!(
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
    backfill(
        client,
        pool,
        indexer,
        shard_id,
        &candidate_peers,
        &target_sth,
    )
    .await
}

/// Fetches and independently verifies every entry between what's already
/// been mirrored locally and `sth.tree_size`, storing only entries whose
/// inclusion actually checks out. Round-robins across `candidate_peers`
/// (every peer that corroborated `sth` this tick, per
/// [`backfill_network`]) for each individual request — if one is
/// unreachable mid-backfill, the next candidate is tried before giving up
/// for this tick, rather than aborting outright. Storage is keyed on
/// `(network_id, shard_id, seq)`, so failing over between peers within the
/// same shard never duplicates or restarts progress, and two different
/// shards' entries — even at the same `seq` — are never confused for each
/// other (see `avalon_chain::mirror::insert_mirrored_entry`'s
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
    shard_id: &str,
    candidate_peers: &[String],
    sth: &SignedTreeHead,
) -> Result<(), MirrorWatcherError> {
    if candidate_peers.is_empty() {
        return Ok(());
    }

    // Issue #604: `shard_id` scopes this to exactly the shard `sth`
    // belongs to — #573 left this deliberately unscoped ("how much of
    // this *network* have I mirrored"), explicitly naming this as a real,
    // separate follow-up question at the time. Confirmed live as a real
    // bug, not just theoretical: a node mirroring more than one shard of
    // the same network had its inclusion-proof verification state
    // silently collide between shards without this.
    let mut progress = mirror::mirrored_progress(pool, &sth.network_id, shard_id, None).await?;
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
            shard_id,
            progress.last_seq,
            BACKFILL_PAGE_SIZE,
        )
        .await
        {
            Ok(result) => result,
            Err(MirrorWatcherError::AllPeersFailed) => {
                tracing::error!(
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
                shard_id,
                entry.seq,
                sth.tree_size,
            )
            .await
            {
                Ok(dto) => dto,
                Err(MirrorWatcherError::AllPeersFailed) => {
                    tracing::error!(
                        "mirror-watcher: {}: every candidate peer failed to serve an inclusion proof for seq={} — retrying next tick",
                        sth.network_id, entry.seq
                    );
                    return Ok(());
                }
                Err(err) => return Err(err),
            };

            if proof_dto.root_hash != sth.root_hash {
                tracing::error!(
                    "mirror-watcher: {}: inclusion-proof root_hash for seq={} did not match the already-verified/corroborated STH root_hash at tree_size={} — aborting backfill this tick",
                    sth.network_id, entry.seq, sth.tree_size
                );
                return Err(MirrorWatcherError::RootHashMismatch);
            }
            if proof_dto.leaf_hash != entry.entry_hash {
                tracing::error!(
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
                tracing::error!(
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
                shard_id: shard_id.to_string(),
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
                        tracing::error!(
                            "mirror-watcher: {}: seq={} verified and mirrored, but the local indexer projection failed ({err}) — likely a core row this replay-only node never independently created; entry is stored, indexer state for it is incomplete",
                            sth.network_id, mirrored_entry.seq
                        );
                    }
                }
            } else {
                tracing::error!(
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
/// `outbox::drain_once` builds from `protocol_outbox` rows.
/// `None` for a pruned payload (retention — the entry is still
/// mirrored, just not applicable to the indexer) or an `issuer`/`subject`
/// that isn't a well-formed `GlobalId`, which should never happen for a
/// genuine ledger entry but is handled as a skip, not a panic, since this is
/// peer-derived content.
fn protocol_event_from_mirrored(entry: &mirror::MirroredEntry) -> Option<ProtocolEvent> {
    let (payload, identity_chain) =
        avalon_protocol::identity_chain_wire::split_position(entry.payload.clone()?);
    Some(ProtocolEvent {
        id: entry.event_id,
        kind: entry.kind.clone(),
        issuer: global_id_from_str(&entry.issuer)?,
        subject: global_id_from_str(&entry.subject)?,
        payload,
        timestamp: entry.event_timestamp,
        version: u32::try_from(entry.version).ok()?,
        identity_chain,
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
    shard_id: &str,
    since_seq: i64,
    limit: i64,
) -> Result<(Vec<LedgerEntryDto>, String), MirrorWatcherError> {
    for offset in 0..candidates.len() {
        let idx = (*cursor + offset) % candidates.len();
        let peer = &candidates[idx];
        match fetch_entries(client, peer, shard_id, since_seq, limit).await {
            Ok(entries) => {
                *cursor = (idx + 1) % candidates.len();
                return Ok((entries, peer.clone()));
            }
            Err(err) => {
                tracing::error!("mirror-watcher: {peer}: GET /ledger/entries failed, trying next candidate peer: {err}");
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
    shard_id: &str,
    seq: i64,
    tree_size: i64,
) -> Result<InclusionProofDto, MirrorWatcherError> {
    for offset in 0..candidates.len() {
        let idx = (*cursor + offset) % candidates.len();
        let peer = &candidates[idx];
        match fetch_inclusion_proof(client, peer, shard_id, seq, tree_size).await {
            Ok(dto) => {
                *cursor = (idx + 1) % candidates.len();
                return Ok(dto);
            }
            Err(err) => {
                tracing::error!("mirror-watcher: {peer}: GET /ledger/proof/inclusion failed, trying next candidate peer: {err}");
            }
        }
    }
    Err(MirrorWatcherError::AllPeersFailed)
}

/// Issue #604: `shard_id` sent as an explicit query param, same #573
/// footgun as [`fetch_latest_sth`] — a peer serving more than one shard
/// answers a bare request with whichever it treats as its own default.
async fn fetch_entries(
    client: &reqwest::Client,
    peer: &str,
    shard_id: &str,
    since_seq: i64,
    limit: i64,
) -> Result<Vec<LedgerEntryDto>, MirrorWatcherError> {
    let url = format!("{peer}/ledger/entries");
    let request = client.get(&url).query(&[
        ("since_seq", since_seq.to_string()),
        ("limit", limit.to_string()),
        ("shard_id", shard_id.to_string()),
    ]);
    let entries: Vec<LedgerEntryDto> = send_with_rate_limit_retry(request)
        .await?
        .error_for_status()?
        .json()
        .await
        .map_err(|e| MirrorWatcherError::Decode(e.to_string()))?;
    Ok(entries)
}

/// Issue #604: `shard_id` sent as an explicit query param — same reason
/// as [`fetch_entries`].
async fn fetch_inclusion_proof(
    client: &reqwest::Client,
    peer: &str,
    shard_id: &str,
    seq: i64,
    tree_size: i64,
) -> Result<InclusionProofDto, MirrorWatcherError> {
    let url = format!("{peer}/ledger/proof/inclusion");
    let request = client.get(&url).query(&[
        ("seq", seq.to_string()),
        ("tree_size", tree_size.to_string()),
        ("shard_id", shard_id.to_string()),
    ]);
    let dto: InclusionProofDto = send_with_rate_limit_retry(request)
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

    #[test]
    fn a_peer_at_the_current_protocol_version_passes() {
        assert!(check_peer_version("http://peer", crate::version::PROTOCOL_VERSION).is_ok());
    }

    #[test]
    fn an_empty_peer_version_is_incompatible_not_a_panic() {
        let err = check_peer_version("http://peer", "").unwrap_err();
        assert!(matches!(
            err,
            MirrorWatcherError::IncompatiblePeerVersion { .. }
        ));
    }

    #[test]
    fn an_unparseable_peer_version_is_incompatible_not_a_panic() {
        let err = check_peer_version("http://peer", "not-a-version").unwrap_err();
        assert!(matches!(
            err,
            MirrorWatcherError::IncompatiblePeerVersion { .. }
        ));
    }

    #[test]
    fn a_peer_version_below_the_floor_is_incompatible() {
        let err = check_peer_version("http://peer", "0.0.1").unwrap_err();
        match err {
            MirrorWatcherError::IncompatiblePeerVersion {
                peer_version,
                floor,
            } => {
                assert_eq!(peer_version, "0.0.1");
                assert_eq!(
                    floor,
                    crate::version::effective_min_peer_version().to_string()
                );
            }
            other => panic!("expected IncompatiblePeerVersion, got {other:?}"),
        }
    }

    fn anchor(
        network_id: &str,
        verify_key_hex: String,
    ) -> avalon_protocol::network_trust::TrustAnchorEntry {
        avalon_protocol::network_trust::TrustAnchorEntry {
            label: network_id.to_string(),
            network_id: network_id.to_string(),
            verify_key: verify_key_hex,
            signing_key_id: "test-key".to_string(),
            server_url: None,
            environment: avalon_protocol::network_trust::NetworkEnvironment::LocalDev,
            seed_nodes: Vec::new(),
            notes: None,
        }
    }

    // Issue #515: a node mirroring peers on two distinct, independently
    // pinned networks must resolve each against its own key, not one
    // shared process-wide key.
    #[test]
    fn verify_key_for_network_resolves_the_matching_pinned_entry_among_several() {
        let key_a = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let key_b = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let anchors = vec![
            anchor("avalon-a", hex::encode(key_a.verifying_key().to_bytes())),
            anchor("avalon-b", hex::encode(key_b.verifying_key().to_bytes())),
        ];

        let resolved = verify_key_for_network(&anchors, "avalon-b").expect("pinned");
        assert_eq!(resolved, key_b.verifying_key());
    }

    // Issue #513: a `network_id` with no pinned trust anchor must never
    // fall back to some other key — there is simply nothing to verify
    // against, so mirroring it is refused outright.
    #[test]
    fn verify_key_for_network_returns_none_for_an_unpinned_network() {
        let key_a = ed25519_dalek::SigningKey::generate(&mut rand::rng());
        let anchors = vec![anchor(
            "avalon-a",
            hex::encode(key_a.verifying_key().to_bytes()),
        )];

        assert!(verify_key_for_network(&anchors, "avalon-unpinned").is_none());
    }

    #[test]
    fn verify_key_for_network_returns_none_for_a_malformed_pinned_key() {
        let anchors = vec![anchor("avalon-a", "not-hex".to_string())];

        assert!(verify_key_for_network(&anchors, "avalon-a").is_none());
    }

    // Both cases live in one test (rather than two `#[test]` fns) because
    // `cargo test` runs tests in the same binary concurrently by default,
    // and both cases mutate the same process-wide `AVALON_MIRROR_PEERS` env
    // var — two separate tests racing on it would be flaky.
    // All `AVALON_MIRROR_PEERS`/`AVALON_MIRROR_ALL_DISCOVERED_SHARDS`
    // cases live in this one test (rather than separate `#[test]` fns),
    // same reasoning as the comment above: `cargo test` runs tests in one
    // binary concurrently by default, and every case here mutates the
    // same process-wide env vars — separate tests racing on them would be
    // flaky.
    #[test]
    fn from_env_reads_peers_and_poll_interval() {
        let _env = crate::test_env::guard();
        // SAFETY: test-only env mutation of vars no other test in this
        // binary touches.
        unsafe {
            std::env::remove_var("AVALON_MIRROR_PEERS");
            std::env::remove_var("AVALON_MIRROR_ALL_DISCOVERED_SHARDS");
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
            vec![
                ("core".to_string(), "http://localhost:8081".to_string()),
                ("core".to_string(), "http://localhost:8082".to_string()),
            ]
        );
        assert_eq!(
            config.poll_interval,
            Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS)
        );
        assert!(!config.auto_mirror_discovered);
        assert!(
            config.known_shard_ids.contains("core"),
            "a bare, un-prefixed AVALON_MIRROR_PEERS entry is the implicit core shard"
        );

        // Issue #599: `AVALON_MIRROR_ALL_DISCOVERED_SHARDS=true` is enough
        // on its own — no `AVALON_MIRROR_PEERS` required — since a node
        // should be able to opt into auto-mirroring with zero explicit
        // peer configuration at all.
        unsafe {
            std::env::remove_var("AVALON_MIRROR_PEERS");
            std::env::set_var("AVALON_MIRROR_ALL_DISCOVERED_SHARDS", "true");
        }
        let config =
            MirrorWatcherConfig::from_env().expect("auto_mirror_discovered alone should be enough");
        assert!(config.peers.is_empty());
        assert!(config.auto_mirror_discovered);
        assert!(config.known_shard_ids.is_empty());

        // Issue #599: `known_shard_ids` picks up a `shard_id=url` entry's
        // shard_id, not just its url.
        unsafe {
            std::env::set_var(
                "AVALON_MIRROR_PEERS",
                "game:ashen-realms=http://shard-a, http://bare-core-peer",
            );
            std::env::remove_var("AVALON_MIRROR_ALL_DISCOVERED_SHARDS");
        }
        let config = MirrorWatcherConfig::from_env().expect("peers were set");
        assert!(config.known_shard_ids.contains("game:ashen-realms"));
        assert!(config.known_shard_ids.contains("core"));

        unsafe {
            std::env::remove_var("AVALON_MIRROR_PEERS");
            std::env::remove_var("AVALON_MIRROR_ALL_DISCOVERED_SHARDS");
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
            shard_id: mirror::CORE_SHARD_ID.to_string(),
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

    async fn live_test_pool() -> PgPool {
        avalon_devenv::load();
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        sqlx::postgres::PgPoolOptions::new()
            .connect(&database_url)
            .await
            .expect("failed to connect to Postgres — is it reachable?")
    }

    /// The equivocation gate ([`backfill_network`]'s first check)
    /// against real Postgres: a network with an unresolved finding
    /// must not have anything written to `mirrored_entries`, even when
    /// handed a well-formed, internally-consistent observation set that
    /// would otherwise corroborate cleanly. This is the "mirror stops
    /// trusting/serving the affected key's current head" acceptance
    /// criterion — exercised directly against the real gate function, not a
    /// re-implementation of its logic.
    #[tokio::test]
    #[ignore]
    async fn backfill_network_refuses_to_proceed_while_an_equivocation_is_unresolved() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let client = reqwest::Client::new();
        let network_id = format!("avalon-test-gate-{}", Uuid::new_v4());

        mirror::record_equivocation(
            &pool,
            &mirror::EquivocationFinding {
                network_id: network_id.clone(),
                shard_id: mirror::CORE_SHARD_ID.to_string(),
                tree_size: 10,
                source_a: "peer-a".to_string(),
                root_hash_a: "aa".repeat(32),
                source_b: "peer-b".to_string(),
                root_hash_b: "bb".repeat(32),
                resolved_at: None,
                resolved_root_hash: None,
            },
        )
        .await
        .expect("record_equivocation failed");

        // A single, internally-consistent observation — if the gate were
        // not checked first, this would corroborate trivially (one peer
        // "agreeing" with itself) and backfill would proceed.
        let observations = vec![(
            "peer-a".to_string(),
            SignedTreeHead {
                tree_size: 10,
                root_hash: "aa".repeat(32),
                network_id: network_id.clone(),
                signing_key_id: "test-key".to_string(),
                signature: "sig".to_string(),
                created_at: OffsetDateTime::UNIX_EPOCH,
            },
        )];

        backfill_network(
            &client,
            &pool,
            &indexer,
            &network_id,
            mirror::CORE_SHARD_ID,
            &observations,
        )
        .await
        .expect("backfill_network should return Ok(()) rather than error when gated");

        let progress = mirror::mirrored_progress(&pool, &network_id, mirror::CORE_SHARD_ID, None)
            .await
            .expect("mirrored_progress failed");
        assert_eq!(
            progress.verified_count, 0,
            "backfill must not write anything while the network has an unresolved equivocation"
        );
    }

    /// Once the finding is resolved, the gate clears and `backfill_network`
    /// proceeds again (calling into real backfill logic, which will attempt
    /// real HTTP requests to the peer URL and fail gracefully — the point
    /// here is only that it gets *past* the gate, not that it completes a
    /// backfill against an unreachable peer).
    #[tokio::test]
    #[ignore]
    async fn backfill_network_proceeds_again_once_the_finding_is_resolved() {
        let pool = live_test_pool().await;
        let indexer = PostgresIndexer::new(pool.clone());
        let client = reqwest::Client::new();
        let network_id = format!("avalon-test-gate-cleared-{}", Uuid::new_v4());

        mirror::record_equivocation(
            &pool,
            &mirror::EquivocationFinding {
                network_id: network_id.clone(),
                shard_id: mirror::CORE_SHARD_ID.to_string(),
                tree_size: 10,
                source_a: "peer-a".to_string(),
                root_hash_a: "aa".repeat(32),
                source_b: "peer-b".to_string(),
                root_hash_b: "bb".repeat(32),
                resolved_at: None,
                resolved_root_hash: None,
            },
        )
        .await
        .expect("record_equivocation failed");

        mirror::resolve_equivocation(
            &pool,
            &network_id,
            mirror::CORE_SHARD_ID,
            10,
            &"aa".repeat(32),
        )
        .await
        .expect("resolve_equivocation failed");

        let unresolved =
            mirror::unresolved_equivocations(&pool, &network_id, mirror::CORE_SHARD_ID)
                .await
                .expect("unresolved_equivocations failed");
        assert!(
            unresolved.is_empty(),
            "gate should be clear after resolution"
        );

        // With no observations at all, backfill_network takes its "no
        // agreement" early return — still proves it got past the
        // equivocation gate (which would otherwise have returned first with
        // a distinct log line) without needing a reachable peer.
        let observations: Vec<(String, SignedTreeHead)> = Vec::new();
        let result = backfill_network(
            &client,
            &pool,
            &indexer,
            &network_id,
            mirror::CORE_SHARD_ID,
            &observations,
        )
        .await;
        assert!(result.is_ok());
    }
}
