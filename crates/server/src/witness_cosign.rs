//! The witness cosigning decision: whether *this* node should cosign a
//! head it has already independently verified (author signature +
//! majority-of-known-list, done by `crate::mirror_watcher` before this
//! module is ever called). See
//! `avalon-docs/protocol/witness-cosigning.md`'s "What
//! a witness checks before cosigning" for the three checks this
//! implements: author-signature verification is already done by the
//! caller; the two genuinely new checks are this module's whole job.
//!
//! **Check 2 (consistency).** This node must have its own record of the
//! last `(tree_size, root_hash)` it cosigned for this exact network/shard
//! (`avalon_chain::mirror::WitnessCheckpoint`), and the new head must
//! extend that checkpoint via a real RFC 6962 consistency proof — fetched
//! fresh from the same peer the head itself came from and verified with
//! `avalon_chain::merkle::verify_consistency_proof`, never trusted from the
//! peer's say-so. No prior checkpoint at all means there is nothing to
//! extend from yet — the legitimate bootstrap case, cosigned
//! unconditionally.
//!
//! **Check 3 (no self-double-cosign).** A head at exactly this node's
//! already-cosigned `tree_size` but a *different* `root_hash` is refused
//! outright — this node has no business cosigning two different claims
//! about the same log position, and refusing here is what keeps
//! equivocation detection sound in the first place rather than merely
//! likely.
//!
//! **The authoring node cosigns its own heads.** A node does not mirror the shard it
//! authors, so [`cosign_own_latest_head`] (driven by [`run_self_cosign_worker`]) feeds its own
//! newest head through the same decision, with the consistency proof computed from its own
//! ledger instead of fetched from a peer. Without it a known list holding only the author
//! and one other witness could never reach majority (#1122).
//!
//! **Independence consequence.** The author's witness key counts as one of the N known
//! witnesses but shares the author's operator, so a forking author can cosign fork A and fork B
//! with it, and each fork can reach majority with honest witnesses that each saw one head. For
//! fork *prevention*, N witnesses means about N-1 independent ones (with {author, other}, the
//! other is the only independent check), so list enough independent witnesses. *Detection* is
//! unchanged: the two author-signed heads at one size still prove equivocation, and honest code
//! never double-cosigns because of the checkpoint guard above.
//!
//! **Check 4, not from the design doc's numbered list but load-bearing
//! anyway:** a shard already marked equivocating
//! (`crate::nodes::HeadGossipTracker::is_equivocating`) gets no further
//! cosignatures from this node until a human resolves the dispute — the
//! consumer that gate was built for and never had until now.

use avalon_chain::mirror::{self, WitnessCheckpoint};
use avalon_chain::{merkle, PostgresSettlementProvider};
use avalon_protocol::cosigned_sth::CosignedTreeHead;
use avalon_protocol::signing_bytes::HashAlgo;
use avalon_protocol::witness::{reattest_witness_cosignature, sign_witness_cosignature};
use ed25519_dalek::SigningKey;
use serde::Deserialize;
use sqlx::PgPool;
use time::OffsetDateTime;

use crate::nodes::HeadGossipTracker;

/// This node's witness-cosigning identity, loaded once at startup —
/// `None` (via [`Self::from_env`]) whenever this node has nothing to
/// cosign with, or has opted out entirely; a node in either state still
/// verifies everything `mirror_watcher` already does, it just never
/// produces a cosignature of its own.
#[derive(Clone)]
pub struct WitnessCosignConfig {
    signing_key: SigningKey,
    witness_key_id: String,
}

impl WitnessCosignConfig {
    pub fn new(signing_key: SigningKey, witness_key_id: String) -> Self {
        Self {
            signing_key,
            witness_key_id,
        }
    }

    pub fn key_id(&self) -> &str {
        &self.witness_key_id
    }

    /// The signer that proves possession of this witness key in announces;
    /// `None` when the key id is not the hex verifying key.
    pub fn announce_signer(&self) -> Option<crate::nodes::WitnessSigner> {
        crate::nodes::WitnessSigner::new(self.signing_key.clone(), self.witness_key_id.clone())
    }

    /// `None` when `AVALON_WITNESS_COSIGNING_ENABLED` is explicitly falsy
    /// (`"false"`/`"0"` — same opt-out convention `AVALON_DHT_ENABLED`
    /// already uses, on by default), or when no witness signing key is
    /// available at all — see
    /// `avalon_protocol::witness::load_witness_signing_key_from_env`'s own
    /// doc comment for the key-domain decision and its settlement-key
    /// fallback. Either way this is logged, never a hard startup failure: a
    /// node with cosigning turned off, or nothing to cosign with, still
    /// runs fine as a pure verifying mirror.
    pub fn from_env() -> Option<Self> {
        let enabled = std::env::var("AVALON_WITNESS_COSIGNING_ENABLED")
            .map(|v| !(v.eq_ignore_ascii_case("false") || v == "0"))
            .unwrap_or(true);
        if !enabled {
            tracing::info!(
                "witness cosigning disabled via AVALON_WITNESS_COSIGNING_ENABLED — this node \
                 will verify but never cosign heads"
            );
            return None;
        }
        match avalon_protocol::witness::load_witness_signing_key_from_env() {
            Ok((signing_key, witness_key_id)) => {
                tracing::info!(
                    witness_key_id = %witness_key_id,
                    "witness cosigning enabled"
                );
                Some(Self {
                    signing_key,
                    witness_key_id,
                })
            }
            Err(err) => {
                tracing::info!(
                    error = %err,
                    "witness cosigning has no signing key available — this node will verify \
                     but never cosign heads"
                );
                None
            }
        }
    }
}

/// The pure part of the cosigning decision — given this node's own last
/// checkpoint (if any) for a log, and a newly-verified head's
/// `tree_size`/`root_hash`, decide what to do next. Split out from the
/// network/storage side so the three-way branch (bootstrap, extend,
/// refuse) is directly unit-testable without a live peer or database.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CheckpointDecision {
    /// No prior checkpoint for this log at all — nothing to extend from
    /// yet, cosign unconditionally and record this as the first checkpoint.
    Bootstrap,
    /// This exact `(tree_size, root_hash)` was already cosigned — a
    /// repeat observation (e.g. re-verified on a later poll tick, or
    /// reported by more than one peer this tick), not a fresh decision.
    AlreadyCosigned,
    /// A *different* root at the exact `tree_size` this node already
    /// cosigned — the no-double-cosign guard. Refused unconditionally,
    /// logged loudly: this is either an attack or a bug, never a normal
    /// operating condition.
    ConflictingRootAtSameSize,
    /// A `tree_size` at or behind the last checkpoint but not identical to
    /// it — a stale/reordered observation, never a legitimate extension.
    NotForward,
    /// A genuinely larger `tree_size` — needs a consistency proof from the
    /// checkpoint to the new head before this node cosigns it.
    NeedsConsistencyProof,
}

fn classify_against_checkpoint(
    checkpoint: Option<&WitnessCheckpoint>,
    tree_size: i64,
    root_hash: &str,
) -> CheckpointDecision {
    let Some(checkpoint) = checkpoint else {
        return CheckpointDecision::Bootstrap;
    };
    match tree_size.cmp(&checkpoint.tree_size) {
        std::cmp::Ordering::Equal => {
            if checkpoint.root_hash == root_hash {
                CheckpointDecision::AlreadyCosigned
            } else {
                CheckpointDecision::ConflictingRootAtSameSize
            }
        }
        std::cmp::Ordering::Less => CheckpointDecision::NotForward,
        std::cmp::Ordering::Greater => CheckpointDecision::NeedsConsistencyProof,
    }
}

/// What one cosigning decision did; lets callers and tests tell the guard branches apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CosignOutcome {
    /// No witness key or cosigning is turned off.
    Disabled,
    /// No head to consider yet.
    NoHead,
    /// The shard has a recorded equivocation, so nothing is cosigned.
    Equivocating,
    /// A new cosignature was produced (or a missing one for the checkpointed head repaired).
    Cosigned,
    /// This exact head was already cosigned.
    AlreadyCosigned,
    /// A different root at an already-cosigned size.
    ConflictingRoot,
    /// A size behind the checkpoint.
    NotForward,
    /// A larger head that does not extend the checkpoint.
    ConsistencyFailed,
    /// A storage read or write failed; retried next tick.
    StorageError,
}

/// How long a repeated, unchanged refusal stays silent before it is logged again.
const REFUSAL_REMINDER: std::time::Duration = std::time::Duration::from_secs(300);
/// Bound on remembered shards so a hostile shard list cannot grow the map without limit.
const REFUSAL_LOG_MAX_SHARDS: usize = 4096;

/// Remembers the last refusal logged per shard so a persistent refusal logs once per change.
#[derive(Default)]
struct RefusalLog {
    last: std::collections::HashMap<String, (String, std::time::Instant)>,
}

impl RefusalLog {
    /// Whether `(shard, state)` should be logged now: first sight, a changed state, or a reminder.
    fn due(&mut self, shard: &str, state: String, now: std::time::Instant) -> bool {
        if self.last.len() >= REFUSAL_LOG_MAX_SHARDS && !self.last.contains_key(shard) {
            self.last.clear();
        }
        match self.last.get(shard) {
            Some((seen, at)) if *seen == state && now.duration_since(*at) < REFUSAL_REMINDER => {
                false
            }
            _ => {
                self.last.insert(shard.to_string(), (state, now));
                true
            }
        }
    }
}

fn refusal_due(shard: &str, state: String) -> bool {
    static LOG: std::sync::OnceLock<std::sync::Mutex<RefusalLog>> = std::sync::OnceLock::new();
    let mut log = LOG
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    log.due(shard, state, std::time::Instant::now())
}

/// Where the consistency proof for a larger head comes from.
#[derive(Clone, Copy)]
enum ProofSource<'a> {
    /// Fetched from, and bound to the roots of, the peer the head came from.
    Peer {
        client: &'a crate::node_http::NodeClient,
        base_url: &'a str,
    },
    /// Computed from this node's own ledger, for the shard it authors.
    OwnLedger,
}

#[derive(Deserialize)]
struct ConsistencyProofDto {
    first_root_hash: String,
    second_root_hash: String,
    proof: Vec<String>,
}

fn decode_root(hex_value: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(hex_value).ok()?;
    <[u8; 32]>::try_from(bytes.as_slice()).ok()
}

fn decode_proof_nodes(hex_nodes: &[String]) -> Option<Vec<[u8; 32]>> {
    hex_nodes
        .iter()
        .map(|node| decode_root(node))
        .collect::<Option<Vec<_>>>()
}

/// Fetches a fresh RFC 6962 consistency proof from `peer_base_url` for
/// `(checkpoint.tree_size, tree_size]` and verifies it locally against
/// `checkpoint.root_hash` (this node's own already-trusted anchor) and
/// `root_hash` (the new head under consideration) — never trusts the
/// peer's own claimed roots, only the two this node already independently
/// holds. `false` for any decode/verification failure, never a panic on
/// peer-controlled input.
async fn verify_consistency_extends_checkpoint(
    client: &crate::node_http::NodeClient,
    peer_base_url: &str,
    shard_id: &str,
    checkpoint: &WitnessCheckpoint,
    tree_size: i64,
    root_hash: &str,
    algo: HashAlgo,
) -> bool {
    let url = format!("{peer_base_url}/ledger/proof/consistency");
    let response = match client
        .get(&url)
        .query(&[
            ("first", checkpoint.tree_size.to_string()),
            ("second", tree_size.to_string()),
            ("shard_id", shard_id.to_string()),
        ])
        .send()
        .await
    {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!(
                peer = %peer_base_url,
                error = %err,
                "witness-cosign: failed to fetch a consistency proof — not cosigning this head",
            );
            return false;
        }
    };
    let dto: ConsistencyProofDto = match response.error_for_status() {
        Ok(response) => match response.json().await {
            Ok(dto) => dto,
            Err(err) => {
                tracing::warn!(
                    peer = %peer_base_url,
                    error = %err,
                    "witness-cosign: unparseable consistency-proof response — not cosigning",
                );
                return false;
            }
        },
        Err(err) => {
            tracing::warn!(
                peer = %peer_base_url,
                error = %err,
                "witness-cosign: peer refused the consistency-proof request — not cosigning",
            );
            return false;
        }
    };

    // The peer's own reported roots must match what this node already
    // independently holds for both ends of the proof — the checkpoint
    // (this node's own prior record) and the new head (already verified by
    // author signature/majority cosignature before this module ever runs).
    // A peer returning a proof against different roots than the ones this
    // node is actually asking about is never trusted, not even if the
    // proof itself would otherwise verify.
    if dto.first_root_hash != checkpoint.root_hash || dto.second_root_hash != root_hash {
        tracing::warn!(
            peer = %peer_base_url,
            "witness-cosign: peer's consistency-proof response claims different roots than this \
             node's own checkpoint/observed head — not cosigning",
        );
        return false;
    }

    let (Some(old_root), Some(new_root), Some(proof)) = (
        decode_root(&dto.first_root_hash),
        decode_root(&dto.second_root_hash),
        decode_proof_nodes(&dto.proof),
    ) else {
        tracing::warn!(
            peer = %peer_base_url,
            "witness-cosign: consistency-proof response contained non-hex data — not cosigning",
        );
        return false;
    };

    merkle::verify_consistency_proof(
        algo,
        checkpoint.tree_size as usize,
        tree_size as usize,
        &proof,
        &old_root,
        &new_root,
    )
}

/// Verifies, from this node's own ledger, that `root_hash` at `tree_size` extends `checkpoint`.
async fn own_ledger_extends_checkpoint(
    chain: &PostgresSettlementProvider,
    checkpoint: &WitnessCheckpoint,
    tree_size: i64,
    root_hash: &str,
    algo: HashAlgo,
) -> bool {
    match chain
        .consistency_proof(checkpoint.tree_size, tree_size)
        .await
    {
        Ok(proof) => proof_extends_checkpoint(algo, checkpoint, tree_size, root_hash, &proof),
        Err(err) => {
            tracing::warn!(error = %err, "witness-cosign: no own-ledger proof, not cosigning");
            false
        }
    }
}

/// Whether `proof` shows `root_hash` at `tree_size` extends `checkpoint`'s own recorded root.
fn proof_extends_checkpoint(
    algo: HashAlgo,
    checkpoint: &WitnessCheckpoint,
    tree_size: i64,
    root_hash: &str,
    proof: &[[u8; 32]],
) -> bool {
    let (Some(old_root), Some(new_root)) =
        (decode_root(&checkpoint.root_hash), decode_root(root_hash))
    else {
        return false;
    };
    merkle::verify_consistency_proof(
        algo,
        checkpoint.tree_size as usize,
        tree_size as usize,
        proof,
        &old_root,
        &new_root,
    )
}

/// The cosigning decision itself — called by `mirror_watcher` right after
/// a head has been independently verified (author signature + majority
/// cosignature of whatever's currently known). `config` being `None` means
/// this node never cosigns anything (see [`WitnessCosignConfig::from_env`]);
/// callers can skip calling this entirely in that case, but this function
/// also accepts it directly so call sites don't need their own `if let
/// Some` wrapper.
///
/// Never panics on peer-controlled input; every failure path just declines
/// to cosign and logs why, exactly like every other verification step in
/// this codebase.
#[allow(clippy::too_many_arguments)]
pub async fn decide_and_cosign(
    chain: &PostgresSettlementProvider,
    pool: &PgPool,
    client: &crate::node_http::NodeClient,
    config: Option<&WitnessCosignConfig>,
    head_gossip: &HeadGossipTracker,
    peer_base_url: &str,
    shard_id: &str,
    head: &CosignedTreeHead,
) {
    let proof = ProofSource::Peer {
        client,
        base_url: peer_base_url,
    };
    let _ = decide(chain, pool, config, head_gossip, proof, shard_id, head).await;
}

/// Cosigns this node's own newest head of the shard it authors, through the same checkpoint,
/// consistency and equivocation guards as a mirrored head. `config` being `None` or no head yet
/// is a no-op; the author signature needs no re-check because this node signed the head itself.
pub async fn cosign_own_latest_head(
    chain: &PostgresSettlementProvider,
    pool: &PgPool,
    config: Option<&WitnessCosignConfig>,
    head_gossip: &HeadGossipTracker,
    own_shard_id: &str,
) -> CosignOutcome {
    if config.is_none() {
        return CosignOutcome::Disabled;
    }
    let sth = match chain.latest_signed_tree_head().await {
        Ok(Some(sth)) => sth,
        Ok(None) => return CosignOutcome::NoHead,
        Err(err) => {
            tracing::error!(error = %err, "witness-cosign: failed to read own latest head");
            return CosignOutcome::StorageError;
        }
    };
    let head = CosignedTreeHead {
        sth,
        cosignatures: Vec::new(),
    };
    let proof = ProofSource::OwnLedger;
    decide(chain, pool, config, head_gossip, proof, own_shard_id, &head).await
}

/// How often the authoring node looks for a new head of its own to cosign.
pub const SELF_COSIGN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// Runs [`cosign_own_latest_head`] on a fixed interval, starting immediately so a restarted node
/// cosigns its existing head right away. Never returns.
pub async fn run_self_cosign_worker(
    chain: PostgresSettlementProvider,
    pool: PgPool,
    config: WitnessCosignConfig,
    head_gossip: HeadGossipTracker,
    own_shard_id: String,
) {
    let mut ticker = tokio::time::interval(SELF_COSIGN_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticker.tick().await;
        let _ =
            cosign_own_latest_head(&chain, &pool, Some(&config), &head_gossip, &own_shard_id).await;
    }
}

async fn decide(
    chain: &PostgresSettlementProvider,
    pool: &PgPool,
    config: Option<&WitnessCosignConfig>,
    head_gossip: &HeadGossipTracker,
    proof: ProofSource<'_>,
    shard_id: &str,
    head: &CosignedTreeHead,
) -> CosignOutcome {
    let Some(config) = config else {
        return CosignOutcome::Disabled;
    };
    let (size, root) = (head.sth.tree_size, &head.sth.root_hash);

    if head_gossip.is_equivocating(shard_id) {
        if refusal_due(shard_id, format!("equivocating:{size}:{root}")) {
            tracing::warn!(
                network_id = %head.sth.network_id,
                shard_id,
                tree_size = size,
                "witness-cosign: shard has a confirmed equivocation on record — refusing to \
                 cosign anything for it until a human resolves the dispute",
            );
        }
        return CosignOutcome::Equivocating;
    }

    let network_id = head.sth.network_id.clone();
    let checkpoint = match mirror::witness_checkpoint_for(pool, &network_id, shard_id).await {
        Ok(checkpoint) => checkpoint,
        Err(err) => {
            tracing::error!(
                network_id = %network_id,
                shard_id,
                error = %err,
                "witness-cosign: failed to read this node's own last-cosigned checkpoint — not \
                 cosigning this tick",
            );
            return CosignOutcome::StorageError;
        }
    };

    match classify_against_checkpoint(checkpoint.as_ref(), size, root) {
        CheckpointDecision::AlreadyCosigned => {
            ensure_own_cosignature_stored(chain, config, &network_id, shard_id, head).await;
            CosignOutcome::AlreadyCosigned
        }
        CheckpointDecision::ConflictingRootAtSameSize => {
            if refusal_due(shard_id, format!("conflict:{size}:{root}")) {
                tracing::error!(
                    event = "witness_double_cosign_refused",
                    network_id = %network_id,
                    shard_id,
                    tree_size = size,
                    new_root_hash = %root,
                    previously_cosigned_root_hash = checkpoint.as_ref().map(|c| c.root_hash.as_str()).unwrap_or_default(),
                    "witness-cosign: refusing to cosign a different root at a tree_size this node \
                     already cosigned — this is exactly the no-double-cosign guarantee holding",
                );
            }
            CosignOutcome::ConflictingRoot
        }
        CheckpointDecision::NotForward => {
            let checkpoint_size = checkpoint.as_ref().map(|c| c.tree_size).unwrap_or_default();
            if refusal_due(shard_id, format!("behind:{size}:{root}:{checkpoint_size}")) {
                tracing::warn!(
                    network_id = %network_id,
                    shard_id,
                    tree_size = size,
                    checkpoint_tree_size = checkpoint_size,
                    "witness-cosign: observed head's tree_size is behind this node's own \
                     checkpoint — stale observation, not cosigning",
                );
            }
            CosignOutcome::NotForward
        }
        CheckpointDecision::Bootstrap => {
            cosign_and_record(chain, pool, config, &network_id, shard_id, head).await
        }
        CheckpointDecision::NeedsConsistencyProof => {
            let Some(checkpoint) = checkpoint else {
                return CosignOutcome::StorageError;
            };
            let extends = match proof {
                ProofSource::Peer { client, base_url } => {
                    verify_consistency_extends_checkpoint(
                        client,
                        base_url,
                        shard_id,
                        &checkpoint,
                        size,
                        root,
                        head.sth.envelope.hash_algo,
                    )
                    .await
                }
                ProofSource::OwnLedger => {
                    own_ledger_extends_checkpoint(
                        chain,
                        &checkpoint,
                        size,
                        root,
                        head.sth.envelope.hash_algo,
                    )
                    .await
                }
            };
            if extends {
                return cosign_and_record(chain, pool, config, &network_id, shard_id, head).await;
            }
            let state = format!("inconsistent:{size}:{root}:{}", checkpoint.tree_size);
            if refusal_due(shard_id, state) {
                tracing::error!(
                    event = "witness_consistency_check_failed",
                    network_id = %network_id,
                    shard_id,
                    checkpoint_tree_size = checkpoint.tree_size,
                    tree_size = size,
                    "witness-cosign: new head did not consistency-proof-extend this node's own \
                     last-cosigned checkpoint — refusing to cosign",
                );
            }
            CosignOutcome::ConsistencyFailed
        }
    }
}

/// Advances this node's own checkpoint, then produces and durably stores
/// its cosignature. The checkpoint moves first on purpose: a crash between
/// the two writes leaves a checkpoint with no cosignature (a withheld
/// cosignature, repaired by [`ensure_own_cosignature_stored`] the next time
/// the same head is seen), never a cosignature with no checkpoint (which
/// would let a later conflicting head at the same size slip past the
/// no-double-cosign check).
async fn cosign_and_record(
    chain: &PostgresSettlementProvider,
    pool: &PgPool,
    config: &WitnessCosignConfig,
    network_id: &str,
    shard_id: &str,
    head: &CosignedTreeHead,
) -> CosignOutcome {
    if let Err(err) = mirror::record_witness_checkpoint(
        pool,
        network_id,
        shard_id,
        head.sth.tree_size,
        &head.sth.root_hash,
        &config.witness_key_id,
    )
    .await
    {
        tracing::error!(
            network_id = %network_id,
            shard_id,
            tree_size = head.sth.tree_size,
            error = %err,
            "witness-cosign: failed to advance this node's own checkpoint — not cosigning",
        );
        return CosignOutcome::StorageError;
    }
    if ensure_own_cosignature_stored(chain, config, network_id, shard_id, head).await {
        CosignOutcome::Cosigned
    } else {
        CosignOutcome::StorageError
    }
}

/// Signs and stores this node's cosignature over `head` unless one is
/// already stored for it. Safe to call for a head the checkpoint already
/// covers: it is the same root at the same size, so this can never produce
/// a second, different cosignature for a tree_size.
async fn ensure_own_cosignature_stored(
    chain: &PostgresSettlementProvider,
    config: &WitnessCosignConfig,
    network_id: &str,
    shard_id: &str,
    head: &CosignedTreeHead,
) -> bool {
    match chain
        .list_witness_cosignatures(network_id, shard_id, head.sth.tree_size)
        .await
    {
        Ok(existing) => {
            if existing
                .iter()
                .any(|c| c.witness_key_id == config.witness_key_id)
            {
                return true;
            }
        }
        Err(err) => {
            tracing::error!(
                network_id = %network_id,
                shard_id,
                error = %err,
                "witness-cosign: failed to read stored cosignatures — not cosigning this tick",
            );
            return false;
        }
    }

    let cosig = match sign_witness_cosignature(
        &config.signing_key,
        &config.witness_key_id,
        &head.sth,
        OffsetDateTime::now_utc(),
    ) {
        Ok(cosig) => cosig,
        Err(err) => {
            tracing::error!(
                network_id = %network_id,
                shard_id,
                error = %err,
                "witness-cosign: cannot sign this head — not cosigning",
            );
            return false;
        }
    };
    if let Err(err) = chain.store_witness_cosignature(shard_id, &cosig).await {
        tracing::error!(
            network_id = %network_id,
            shard_id,
            tree_size = head.sth.tree_size,
            error = %err,
            "witness-cosign: failed to durably store this node's own cosignature",
        );
        return false;
    }
    tracing::info!(
        event = "witness_cosigned",
        network_id = %network_id,
        shard_id,
        tree_size = head.sth.tree_size,
        root_hash = %head.sth.root_hash,
        witness_key_id = %config.witness_key_id,
        "witness-cosign: cosigned a newly-verified head",
    );
    true
}

/// How often this witness re-attests its current head; a third of the cosignature freshness
/// window unless `AVALON_WITNESS_REATTEST_SECS` overrides it.
pub fn reattest_interval_from_env() -> std::time::Duration {
    let default = crate::cosign_verify::COSIGNATURE_FRESHNESS_WINDOW / 3;
    std::env::var("AVALON_WITNESS_REATTEST_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .map(std::time::Duration::from_secs)
        .unwrap_or(default)
}

/// A new cosignature over the same head as `existing` with a fresh `observed_at`.
fn reattested(
    config: &WitnessCosignConfig,
    existing: &avalon_protocol::witness::WitnessCosignature,
    now: OffsetDateTime,
) -> Option<avalon_protocol::witness::WitnessCosignature> {
    reattest_witness_cosignature(&config.signing_key, &config.witness_key_id, existing, now).ok()
}

/// Refreshes this node's cosignature over the head it last cosigned for every log it holds a
/// checkpoint for, so an unchanged head keeps a fresh attestation. Returns how many were
/// refreshed. Logs with a recorded equivocation are skipped.
pub async fn reattest_once(
    chain: &PostgresSettlementProvider,
    pool: &PgPool,
    config: &WitnessCosignConfig,
    head_gossip: &HeadGossipTracker,
    now: OffsetDateTime,
) -> usize {
    let checkpoints = match mirror::all_witness_checkpoints(pool).await {
        Ok(checkpoints) => checkpoints,
        Err(err) => {
            tracing::error!(error = %err, "witness-reattest: failed to read checkpoints");
            return 0;
        }
    };
    let mut refreshed = 0;
    for checkpoint in checkpoints {
        if checkpoint.witness_key_id != config.witness_key_id
            || head_gossip.is_equivocating(&checkpoint.shard_id)
        {
            continue;
        }
        let existing = match chain
            .list_witness_cosignatures(
                &checkpoint.network_id,
                &checkpoint.shard_id,
                checkpoint.tree_size,
            )
            .await
        {
            Ok(list) => list,
            Err(err) => {
                tracing::error!(error = %err, "witness-reattest: failed to read cosignatures");
                continue;
            }
        };
        let Some(own) = existing.iter().find(|c| {
            c.witness_key_id == config.witness_key_id && c.root_hash == checkpoint.root_hash
        }) else {
            continue;
        };
        let Some(fresh) = reattested(config, own, now) else {
            continue;
        };
        match chain
            .refresh_witness_cosignature(&checkpoint.shard_id, &fresh)
            .await
        {
            Ok(true) => refreshed += 1,
            Ok(false) => {}
            Err(err) => {
                tracing::error!(
                    shard_id = %checkpoint.shard_id,
                    error = %err,
                    "witness-reattest: failed to store the refreshed cosignature",
                );
            }
        }
    }
    refreshed
}

/// Runs [`reattest_once`] on a fixed interval. Never returns.
pub async fn run_reattest_worker(
    chain: PostgresSettlementProvider,
    pool: PgPool,
    config: WitnessCosignConfig,
    head_gossip: HeadGossipTracker,
    interval: std::time::Duration,
) {
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await;
    loop {
        ticker.tick().await;
        let n = reattest_once(
            &chain,
            &pool,
            &config,
            &head_gossip,
            OffsetDateTime::now_utc(),
        )
        .await;
        if n > 0 {
            tracing::debug!(refreshed = n, "witness-reattest: refreshed cosignatures");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(tree_size: i64, root_hash: &str) -> WitnessCheckpoint {
        WitnessCheckpoint {
            network_id: "avalon-test".to_string(),
            shard_id: "core".to_string(),
            tree_size,
            root_hash: root_hash.to_string(),
            witness_key_id: "witness-1".to_string(),
            cosigned_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn no_checkpoint_is_the_bootstrap_case() {
        assert_eq!(
            classify_against_checkpoint(None, 1, "root-a"),
            CheckpointDecision::Bootstrap
        );
    }

    #[test]
    fn the_exact_same_head_already_cosigned_is_a_no_op() {
        let cp = checkpoint(5, "root-a");
        assert_eq!(
            classify_against_checkpoint(Some(&cp), 5, "root-a"),
            CheckpointDecision::AlreadyCosigned
        );
    }

    #[test]
    fn a_different_root_at_the_same_tree_size_is_a_double_cosign_conflict() {
        let cp = checkpoint(5, "root-a");
        assert_eq!(
            classify_against_checkpoint(Some(&cp), 5, "root-b"),
            CheckpointDecision::ConflictingRootAtSameSize
        );
    }

    #[test]
    fn a_smaller_tree_size_than_the_checkpoint_is_not_forward() {
        let cp = checkpoint(5, "root-a");
        assert_eq!(
            classify_against_checkpoint(Some(&cp), 3, "root-z"),
            CheckpointDecision::NotForward
        );
    }

    #[test]
    fn a_larger_tree_size_needs_a_consistency_proof() {
        let cp = checkpoint(5, "root-a");
        assert_eq!(
            classify_against_checkpoint(Some(&cp), 9, "root-b"),
            CheckpointDecision::NeedsConsistencyProof
        );
    }

    #[test]
    fn cosigning_can_be_disabled_via_the_env_flag() {
        let _env = crate::test_env::guard();
        unsafe {
            std::env::set_var("AVALON_WITNESS_SIGNING_KEY", hex::encode([5u8; 32]));
            std::env::remove_var("AVALON_WITNESS_COSIGNING_ENABLED");
        }
        assert!(WitnessCosignConfig::from_env().is_some());

        for falsy in ["false", "FALSE", "0"] {
            unsafe {
                std::env::set_var("AVALON_WITNESS_COSIGNING_ENABLED", falsy);
            }
            assert!(WitnessCosignConfig::from_env().is_none(), "{falsy}");
        }

        unsafe {
            std::env::remove_var("AVALON_WITNESS_COSIGNING_ENABLED");
            std::env::remove_var("AVALON_WITNESS_SIGNING_KEY");
        }
    }

    #[test]
    fn decode_root_rejects_non_hex_and_wrong_length() {
        assert!(decode_root("not-hex").is_none());
        assert!(decode_root("ab").is_none());
        assert!(decode_root(&"ab".repeat(32)).is_some());
    }

    #[test]
    fn decode_proof_nodes_rejects_any_malformed_entry() {
        let good = "ab".repeat(32);
        assert!(decode_proof_nodes(&[good.clone(), good.clone()]).is_some());
        assert!(decode_proof_nodes(&[good, "not-hex".to_string()]).is_none());
    }

    #[test]
    fn an_unchanged_head_stays_verifiable_past_the_freshness_window_when_reattested() {
        use avalon_protocol::cosigned_sth::{verify_cosigned_tree_head, CosignedTreeHead};
        use avalon_protocol::sth::sign_tree_head;
        use avalon_protocol::witness::sign_witness_cosignature;

        let window = time::Duration::seconds(600);
        let author = SigningKey::from_bytes(&[1u8; 32]);
        let cfg_keys: Vec<WitnessCosignConfig> = (2u8..5)
            .map(|b| {
                let k = SigningKey::from_bytes(&[b; 32]);
                let id = hex::encode(k.verifying_key().to_bytes());
                WitnessCosignConfig::new(k, id)
            })
            .collect();
        let t0 = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
        let sth = sign_tree_head(&author, "author-key", 7, &"ab".repeat(32), "net", t0).unwrap();
        let known: Vec<(String, ed25519_dalek::VerifyingKey)> = cfg_keys
            .iter()
            .map(|c| (c.witness_key_id.clone(), c.signing_key.verifying_key()))
            .collect();
        let mut cosigs: Vec<_> = cfg_keys
            .iter()
            .map(|c| sign_witness_cosignature(&c.signing_key, &c.witness_key_id, &sth, t0).unwrap())
            .collect();
        let accepts = |cosigs: &[_], now: OffsetDateTime| {
            let head = CosignedTreeHead {
                sth: sth.clone(),
                cosignatures: cosigs.to_vec(),
            };
            verify_cosigned_tree_head(&author.verifying_key(), &head, &known, now - window, now)
        };

        let mut now = t0;
        assert!(accepts(&cosigs, now));
        for _ in 0..6 {
            now += window / 3;
            cosigs = cfg_keys
                .iter()
                .zip(&cosigs)
                .map(|(c, old)| reattested(c, old, now).unwrap())
                .collect();
            assert!(accepts(&cosigs, now));
        }
        assert!(now - t0 > window);
    }

    #[test]
    fn without_reattestation_the_same_head_stops_verifying() {
        use avalon_protocol::cosigned_sth::{verify_cosigned_tree_head, CosignedTreeHead};
        use avalon_protocol::sth::sign_tree_head;
        use avalon_protocol::witness::sign_witness_cosignature;

        let window = time::Duration::seconds(600);
        let author = SigningKey::from_bytes(&[1u8; 32]);
        let witnesses: Vec<(String, SigningKey)> = (2u8..5)
            .map(|b| {
                let k = SigningKey::from_bytes(&[b; 32]);
                (hex::encode(k.verifying_key().to_bytes()), k)
            })
            .collect();
        let known: Vec<_> = witnesses
            .iter()
            .map(|(id, k)| (id.clone(), k.verifying_key()))
            .collect();
        let t0 = OffsetDateTime::UNIX_EPOCH + time::Duration::days(1);
        let sth = sign_tree_head(&author, "author-key", 7, &"ab".repeat(32), "net", t0).unwrap();
        let cosignatures = witnesses
            .iter()
            .map(|(id, k)| sign_witness_cosignature(k, id, &sth, t0).unwrap())
            .collect();
        let head = CosignedTreeHead { sth, cosignatures };
        let now = t0 + window * 2;
        assert!(!verify_cosigned_tree_head(
            &author.verifying_key(),
            &head,
            &known,
            now - window,
            now
        ));
    }

    #[test]
    fn own_ledger_proof_accepts_an_extension_and_refuses_a_fork() {
        let leaves: Vec<String> = (0u8..12).map(|i| hex::encode([i; 32])).collect();
        let root = |n: usize| {
            hex::encode(
                merkle::mth_of_hex_hashes(avalon_chain::ledger_hash_algo(), &leaves[..n]).unwrap(),
            )
        };
        let cp = checkpoint(5, &root(5));
        let proof = merkle::consistency_proof_of_hex_hashes(
            avalon_chain::ledger_hash_algo(),
            5,
            &leaves[..12],
        )
        .unwrap();
        assert!(proof_extends_checkpoint(
            avalon_chain::ledger_hash_algo(),
            &cp,
            12,
            &root(12),
            &proof
        ));

        let mut forked = leaves.clone();
        forked[2] = hex::encode([0xEEu8; 32]);
        let forked_root = hex::encode(
            merkle::mth_of_hex_hashes(avalon_chain::ledger_hash_algo(), &forked[..12]).unwrap(),
        );
        assert!(!proof_extends_checkpoint(
            avalon_chain::ledger_hash_algo(),
            &cp,
            12,
            &forked_root,
            &proof
        ));
        assert!(!proof_extends_checkpoint(
            avalon_chain::ledger_hash_algo(),
            &checkpoint(5, "zz"),
            12,
            &root(12),
            &proof
        ));
    }

    #[test]
    fn a_persistent_refusal_logs_once_then_only_as_a_reminder_or_on_change() {
        use std::time::{Duration, Instant};
        let mut log = RefusalLog::default();
        let t0 = Instant::now();
        assert!(log.due("core", "behind:5".into(), t0));
        assert!(!log.due("core", "behind:5".into(), t0 + Duration::from_secs(2)));
        assert!(log.due("core", "behind:6".into(), t0 + Duration::from_secs(4)));
        assert!(log.due("other", "behind:6".into(), t0 + Duration::from_secs(4)));
        assert!(!log.due("core", "behind:6".into(), t0 + Duration::from_secs(60)));
        assert!(log.due(
            "core",
            "behind:6".into(),
            t0 + REFUSAL_REMINDER + Duration::from_secs(5)
        ));
    }

    #[tokio::test]
    async fn a_two_witness_list_trusts_the_head_only_once_the_author_cosigns_it() {
        use crate::cosign_gather::{resolve_majority, HeadVerdict};
        use avalon_protocol::sth::sign_tree_head;

        let author = SigningKey::from_bytes(&[1u8; 32]);
        // A separate witness key, as on a node whose witness key is not its settlement key.
        let author_witness = SigningKey::from_bytes(&[3u8; 32]);
        let author_cfg = {
            let id = hex::encode(author_witness.verifying_key().to_bytes());
            WitnessCosignConfig::new(author_witness.clone(), id)
        };
        let other = SigningKey::from_bytes(&[2u8; 32]);
        let other_id = hex::encode(other.verifying_key().to_bytes());
        let known = vec![
            (other_id.clone(), other.verifying_key()),
            (
                author_cfg.witness_key_id.clone(),
                author_witness.verifying_key(),
            ),
        ];
        let now = OffsetDateTime::now_utc();
        let sth = sign_tree_head(&author, "author-key", 7, &"ab".repeat(32), "net", now).unwrap();
        let cosign = |k: &SigningKey, id: &str| sign_witness_cosignature(k, id, &sth, now).unwrap();
        let resolve = |cosignatures| {
            let head = CosignedTreeHead {
                sth: sth.clone(),
                cosignatures,
            };
            let policy = crate::outbound_policy::OutboundPolicy::new(true);
            let (key, known) = (author.verifying_key(), &known);
            async move { resolve_majority(policy, &key, head, known, &[], "core").await }
        };

        let without_author = vec![cosign(&other, &other_id)];
        assert!(matches!(
            resolve(without_author.clone()).await,
            HeadVerdict::Held
        ));
        let mut both = without_author;
        both.push(cosign(&author_cfg.signing_key, &author_cfg.witness_key_id));
        assert!(matches!(resolve(both).await, HeadVerdict::Trusted(_)));
    }

    #[test]
    fn reattest_interval_defaults_to_a_third_of_the_freshness_window() {
        let _env = crate::test_env::guard();
        unsafe { std::env::remove_var("AVALON_WITNESS_REATTEST_SECS") };
        assert_eq!(
            reattest_interval_from_env(),
            crate::cosign_verify::COSIGNATURE_FRESHNESS_WINDOW / 3
        );
        unsafe { std::env::set_var("AVALON_WITNESS_REATTEST_SECS", "45") };
        assert_eq!(
            reattest_interval_from_env(),
            std::time::Duration::from_secs(45)
        );
        unsafe { std::env::remove_var("AVALON_WITNESS_REATTEST_SECS") };
    }
}
