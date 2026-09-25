//! The witness cosigning decision: whether *this* node should cosign a
//! head it has already independently verified (author signature +
//! majority-of-known-list, done by `crate::mirror_watcher` before this
//! module is ever called). See
//! `docs/projects/backend-server/architecture/witness-cosigning.md`'s "What
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
//! **Check 4, not from the design doc's numbered list but load-bearing
//! anyway:** a shard already marked equivocating
//! (`crate::nodes::HeadGossipTracker::is_equivocating`) gets no further
//! cosignatures from this node until a human resolves the dispute — the
//! consumer that gate was built for and never had until now.

use avalon_chain::mirror::{self, WitnessCheckpoint};
use avalon_chain::{merkle, PostgresSettlementProvider};
use avalon_protocol::cosigned_sth::CosignedTreeHead;
use avalon_protocol::witness::sign_witness_cosignature;
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
    client: &reqwest::Client,
    peer_base_url: &str,
    shard_id: &str,
    checkpoint: &WitnessCheckpoint,
    tree_size: i64,
    root_hash: &str,
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
        checkpoint.tree_size as usize,
        tree_size as usize,
        &proof,
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
    client: &reqwest::Client,
    config: Option<&WitnessCosignConfig>,
    head_gossip: &HeadGossipTracker,
    peer_base_url: &str,
    shard_id: &str,
    head: &CosignedTreeHead,
) {
    let Some(config) = config else {
        return;
    };

    if head_gossip.is_equivocating(shard_id) {
        tracing::warn!(
            network_id = %head.sth.network_id,
            shard_id,
            tree_size = head.sth.tree_size,
            "witness-cosign: shard has a confirmed equivocation on record — refusing to cosign \
             anything for it until a human resolves the dispute",
        );
        return;
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
            return;
        }
    };

    match classify_against_checkpoint(checkpoint.as_ref(), head.sth.tree_size, &head.sth.root_hash)
    {
        CheckpointDecision::AlreadyCosigned => {
            ensure_own_cosignature_stored(chain, config, &network_id, shard_id, head).await;
        }
        CheckpointDecision::ConflictingRootAtSameSize => {
            tracing::error!(
                event = "witness_double_cosign_refused",
                network_id = %network_id,
                shard_id,
                tree_size = head.sth.tree_size,
                new_root_hash = %head.sth.root_hash,
                previously_cosigned_root_hash = checkpoint.as_ref().map(|c| c.root_hash.as_str()).unwrap_or_default(),
                "witness-cosign: refusing to cosign a different root at a tree_size this node \
                 already cosigned — this is exactly the no-double-cosign guarantee holding",
            );
        }
        CheckpointDecision::NotForward => {
            tracing::warn!(
                network_id = %network_id,
                shard_id,
                tree_size = head.sth.tree_size,
                checkpoint_tree_size = checkpoint.as_ref().map(|c| c.tree_size).unwrap_or_default(),
                "witness-cosign: observed head's tree_size is behind this node's own checkpoint \
                 — stale observation, not cosigning",
            );
        }
        CheckpointDecision::Bootstrap => {
            cosign_and_record(chain, pool, config, &network_id, shard_id, head).await;
        }
        CheckpointDecision::NeedsConsistencyProof => {
            let Some(checkpoint) = checkpoint else {
                return;
            };
            let extends = verify_consistency_extends_checkpoint(
                client,
                peer_base_url,
                shard_id,
                &checkpoint,
                head.sth.tree_size,
                &head.sth.root_hash,
            )
            .await;
            if extends {
                cosign_and_record(chain, pool, config, &network_id, shard_id, head).await;
            } else {
                tracing::error!(
                    event = "witness_consistency_check_failed",
                    network_id = %network_id,
                    shard_id,
                    checkpoint_tree_size = checkpoint.tree_size,
                    tree_size = head.sth.tree_size,
                    "witness-cosign: new head did not consistency-proof-extend this node's own \
                     last-cosigned checkpoint — refusing to cosign",
                );
            }
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
) {
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
        return;
    }
    ensure_own_cosignature_stored(chain, config, network_id, shard_id, head).await;
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
) {
    match chain
        .list_witness_cosignatures(network_id, shard_id, head.sth.tree_size)
        .await
    {
        Ok(existing) => {
            if existing
                .iter()
                .any(|c| c.witness_key_id == config.witness_key_id)
            {
                return;
            }
        }
        Err(err) => {
            tracing::error!(
                network_id = %network_id,
                shard_id,
                error = %err,
                "witness-cosign: failed to read stored cosignatures — not cosigning this tick",
            );
            return;
        }
    }

    let cosig = sign_witness_cosignature(
        &config.signing_key,
        &config.witness_key_id,
        head.sth.tree_size,
        &head.sth.root_hash,
        network_id,
        head.sth.created_at,
        OffsetDateTime::now_utc(),
    );
    if let Err(err) = chain.store_witness_cosignature(shard_id, &cosig).await {
        tracing::error!(
            network_id = %network_id,
            shard_id,
            tree_size = head.sth.tree_size,
            error = %err,
            "witness-cosign: failed to durably store this node's own cosignature",
        );
        return;
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
}
