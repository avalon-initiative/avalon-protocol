//! Cross-shard commitment (issue #529, implementing #527's decided
//! sharded-settlement design). See `docs/architecture/settlement.md`'s
//! "Cross-shard commitment" section for the full design this
//! implements — this module is the aggregation math alone, deliberately
//! pure and DB-free (no network I/O, no Postgres): given the set of
//! shard STHs a node currently knows about (however it gathered them —
//! local computation, peer gossip, a test fixture), anyone can compute
//! the identical [`CrossShardRoot`]. That determinism, not any privileged
//! role, is what makes aggregation safe to decentralize — see
//! [`compute_cross_shard_root`]'s own doc comment.
//!
//! Gathering the actual `(shard_id, SignedTreeHead)` pairs this module
//! consumes — via #362's peer gossip, or a shard's own `/ledger/sth/latest`
//! — is `avalon-server`'s concern (`crate::cross_shard` there), not this
//! crate's.

use crate::merkle;
use crate::sth::SignedTreeHead;

/// One `(shard_id, SignedTreeHead)` pair — a shard's current claimed
/// state, as gossiped/fetched from wherever a node learned about it. The
/// caller is responsible for having already verified `sth`'s signature
/// against that shard's own registered key (see
/// `docs/architecture/network-trust-anchors.md`'s "Per-shard trust
/// anchors" section, #543) before handing it to this module — aggregation
/// itself does not re-verify shard-level authenticity, only combines
/// already-trusted inputs deterministically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardTreeHead {
    pub shard_id: String,
    pub sth: SignedTreeHead,
}

/// The result of aggregating every shard STH a node currently knows about
/// — deliberately **not** itself signed by anyone (see module/function
/// doc comments for why). `computed_at` is this node's own wall-clock
/// time of computation, informational only — it is not part of what any
/// hash commits to, since two nodes computing at different instants must
/// still agree on `root_hash` given the same input set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossShardRoot {
    pub root_hash: String,
    pub shard_count: usize,
    #[allow(dead_code)]
    pub computed_at: time::OffsetDateTime,
    /// `true` when this node knows of at least one registered shard it
    /// currently holds no STH for — see [`compute_cross_shard_root_checked`].
    /// A `partial` root is computed over exactly the shards it *does*
    /// have, never silently treated as complete.
    pub partial: bool,
    /// Every `shard_id` this node knows is registered but has no current
    /// STH for — empty unless `partial` is `true`.
    pub missing_shard_ids: Vec<String>,
}

/// The exact leaf-hash construction #529's design specifies:
/// `SHA-256(shard_id ‖ sth.tree_size ‖ sth.root_hash ‖ sth.signing_key_id
/// ‖ sth.signature)` — length-prefixed fields (same domain-separation
/// discipline `sth::signing_message` and `postgres::hash_event` already
/// use in this codebase) so there is exactly one way to encode a given
/// tuple, never an ambiguous concatenation. Committing the STH's own
/// `signature` (not just its `root_hash`) into the leaf is what makes a
/// shard silently downgrading to an earlier, still-validly-signed STH
/// detectable via this tree too, not only via the per-shard log's own
/// consistency proofs.
fn shard_leaf_bytes(shard_id: &str, sth: &SignedTreeHead) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"avalon-cross-shard-leaf-v1");
    bytes.extend_from_slice(&(shard_id.len() as u32).to_be_bytes());
    bytes.extend_from_slice(shard_id.as_bytes());
    bytes.extend_from_slice(&sth.tree_size.to_be_bytes());
    bytes.extend_from_slice(&(sth.root_hash.len() as u32).to_be_bytes());
    bytes.extend_from_slice(sth.root_hash.as_bytes());
    bytes.extend_from_slice(&(sth.signing_key_id.len() as u32).to_be_bytes());
    bytes.extend_from_slice(sth.signing_key_id.as_bytes());
    bytes.extend_from_slice(&(sth.signature.len() as u32).to_be_bytes());
    bytes.extend_from_slice(sth.signature.as_bytes());
    bytes
}

/// Sorts `shards` by `shard_id`, byte-wise ascending (#529's canonical
/// ordering rule — the thing that makes two independent nodes with the
/// same input *set* always produce the same leaf *order*, and therefore
/// the same tree, regardless of the order they happened to learn about
/// each shard in). Deliberately takes ownership and sorts in place rather
/// than requiring the caller to pre-sort — canonical order is this
/// module's invariant to guarantee, not every caller's responsibility to
/// remember.
fn canonical_order(mut shards: Vec<ShardTreeHead>) -> Vec<ShardTreeHead> {
    shards.sort_by(|a, b| a.shard_id.cmp(&b.shard_id));
    shards
}

/// The aggregation recipe itself: sort `shards` canonically, leaf-hash
/// each per [`shard_leaf_bytes`], and combine with the exact same RFC 6962
/// tree-hashing [`merkle::mth`] the single-shard Merkle tree already
/// uses — no new hashing scheme, just a second tree of the same shape,
/// one level up. Two nodes calling this with the same *set* of
/// `ShardTreeHead`s (regardless of the `Vec`'s incoming order) always get
/// back the identical `root_hash` — that determinism is what lets
/// aggregation be redone by anyone rather than delegated to one
/// designated party (#527's own invariant).
///
/// A network with exactly one shard degenerates to a tree over a single
/// leaf — trivially valid, not a special case in this function.
pub fn compute_cross_shard_root(
    shards: Vec<ShardTreeHead>,
    computed_at: time::OffsetDateTime,
) -> CrossShardRoot {
    let ordered = canonical_order(shards);
    let leaves: Vec<Vec<u8>> = ordered
        .iter()
        .map(|s| shard_leaf_bytes(&s.shard_id, &s.sth))
        .collect();
    let root = merkle::mth(&leaves);
    CrossShardRoot {
        root_hash: hex::encode(root),
        shard_count: ordered.len(),
        computed_at,
        partial: false,
        missing_shard_ids: Vec::new(),
    }
}

/// [`compute_cross_shard_root`], but first checks `known_shard_ids`
/// (every shard this node has ever seen register itself) against which
/// shards `shards` actually has an STH for — #529's "detecting a missing
/// or stale shard, instead of silently disagreeing" requirement. A
/// `known` shard with no corresponding entry in `shards` marks the
/// result `partial: true` and names the gap in `missing_shard_ids`,
/// rather than silently computing a root over whatever subset happened
/// to be on hand. The root itself is still computed over exactly the
/// shards actually present — a `partial` result is not "wrong," it's
/// "known to be incomplete," and callers should treat it as non-
/// authoritative for the shards it's missing while still trusting it for
/// the shards it does include.
pub fn compute_cross_shard_root_checked(
    known_shard_ids: &std::collections::BTreeSet<String>,
    shards: Vec<ShardTreeHead>,
    computed_at: time::OffsetDateTime,
) -> CrossShardRoot {
    let present: std::collections::BTreeSet<String> =
        shards.iter().map(|s| s.shard_id.clone()).collect();
    let mut missing: Vec<String> = known_shard_ids.difference(&present).cloned().collect();
    missing.sort();
    let partial = !missing.is_empty();

    let mut result = compute_cross_shard_root(shards, computed_at);
    result.partial = partial;
    result.missing_shard_ids = missing;
    result
}

/// A Merkle inclusion proof that `shard_id`'s STH is part of the given
/// (already-sorted-by-caller-into-`shards`) set's cross-shard root —
/// #529's "no new proof type; the cross-shard tree is verified exactly
/// like the per-shard tree is" invariant: this is a thin wrapper handing
/// the same leaf bytes to `merkle::inclusion_proof` any per-shard log
/// already uses. Returns `None` if `shard_id` isn't present in `shards`.
pub fn inclusion_proof_for_shard(
    shards: &[ShardTreeHead],
    shard_id: &str,
) -> Option<Result<Vec<[u8; 32]>, String>> {
    let ordered = canonical_order(shards.to_vec());
    let index = ordered.iter().position(|s| s.shard_id == shard_id)?;
    let leaves: Vec<Vec<u8>> = ordered
        .iter()
        .map(|s| shard_leaf_bytes(&s.shard_id, &s.sth))
        .collect();
    Some(merkle::inclusion_proof(index, &leaves))
}

/// Verifies a proof produced by [`inclusion_proof_for_shard`] against a
/// specific `root_hash`/`tree_size` (the shard count the proof's tree was
/// built over) and the claimed `(shard_id, sth)` pair — the same
/// `merkle::verify_inclusion_proof` any per-shard inclusion proof already
/// uses, applied one level up. `leaf_index` must be the same index the
/// leaf held in the canonically-sorted set the proof was generated
/// against (a verifier that doesn't already know the full sorted list
/// gets this from whoever handed it the proof, same as any inclusion
/// proof's `seq`).
pub fn verify_shard_inclusion(
    root_hash: &str,
    tree_size: usize,
    leaf_index: usize,
    shard_id: &str,
    sth: &SignedTreeHead,
    proof: &[[u8; 32]],
) -> Result<bool, String> {
    let leaf = shard_leaf_bytes(shard_id, sth);
    let root_bytes = hex::decode(root_hash).map_err(|e| e.to_string())?;
    let root: [u8; 32] = root_bytes
        .try_into()
        .map_err(|_| "root_hash must be exactly 32 bytes".to_string())?;
    Ok(merkle::verify_inclusion_proof(
        &leaf, leaf_index, tree_size, proof, &root,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sth(root_hash: &str, tree_size: i64) -> SignedTreeHead {
        SignedTreeHead {
            tree_size,
            root_hash: root_hash.to_string(),
            network_id: "avalon-test".to_string(),
            signing_key_id: "k1".to_string(),
            signature: "deadbeef".to_string(),
            created_at: time::OffsetDateTime::from_unix_timestamp(0).unwrap(),
        }
    }

    #[test]
    fn same_set_different_incoming_order_produces_the_identical_root() {
        let a = ShardTreeHead {
            shard_id: "game:ashen-realms".to_string(),
            sth: sth("aa", 5),
        };
        let b = ShardTreeHead {
            shard_id: "core".to_string(),
            sth: sth("bb", 10),
        };
        let c = ShardTreeHead {
            shard_id: "app:some-app".to_string(),
            sth: sth("cc", 3),
        };
        let now = time::OffsetDateTime::from_unix_timestamp(0).unwrap();

        let root_1 = compute_cross_shard_root(vec![a.clone(), b.clone(), c.clone()], now).root_hash;
        let root_2 = compute_cross_shard_root(vec![c, a, b], now).root_hash;

        assert_eq!(
            root_1, root_2,
            "two nodes given the same shard STH set (in different incoming order) must \
             compute the identical cross-shard root"
        );
    }

    #[test]
    fn a_different_sth_for_the_same_shard_changes_the_root() {
        let now = time::OffsetDateTime::from_unix_timestamp(0).unwrap();
        let shards_a = vec![ShardTreeHead {
            shard_id: "core".to_string(),
            sth: sth("aa", 5),
        }];
        let shards_b = vec![ShardTreeHead {
            shard_id: "core".to_string(),
            sth: sth("aa", 6), // different tree_size, same root_hash text
        }];

        let root_a = compute_cross_shard_root(shards_a, now).root_hash;
        let root_b = compute_cross_shard_root(shards_b, now).root_hash;
        assert_ne!(
            root_a, root_b,
            "a shard downgrading/changing its STH must change the cross-shard root"
        );
    }

    #[test]
    fn one_shard_degenerates_to_a_single_leaf_tree() {
        let now = time::OffsetDateTime::from_unix_timestamp(0).unwrap();
        let result = compute_cross_shard_root(
            vec![ShardTreeHead {
                shard_id: "core".to_string(),
                sth: sth("aa", 5),
            }],
            now,
        );
        assert_eq!(result.shard_count, 1);
        assert!(!result.partial);
    }

    #[test]
    fn a_missing_known_shard_marks_the_result_partial() {
        let now = time::OffsetDateTime::from_unix_timestamp(0).unwrap();
        let known: std::collections::BTreeSet<String> =
            ["core".to_string(), "game:ashen-realms".to_string()]
                .into_iter()
                .collect();
        let only_core = vec![ShardTreeHead {
            shard_id: "core".to_string(),
            sth: sth("aa", 5),
        }];

        let result = compute_cross_shard_root_checked(&known, only_core, now);
        assert!(result.partial);
        assert_eq!(result.missing_shard_ids, vec!["game:ashen-realms"]);
        // Still computed over exactly the shard(s) actually present.
        assert_eq!(result.shard_count, 1);
    }

    #[test]
    fn no_missing_known_shards_is_not_partial() {
        let now = time::OffsetDateTime::from_unix_timestamp(0).unwrap();
        let known: std::collections::BTreeSet<String> = ["core".to_string()].into_iter().collect();
        let shards = vec![ShardTreeHead {
            shard_id: "core".to_string(),
            sth: sth("aa", 5),
        }];

        let result = compute_cross_shard_root_checked(&known, shards, now);
        assert!(!result.partial);
        assert!(result.missing_shard_ids.is_empty());
    }

    #[test]
    fn inclusion_proof_for_a_present_shard_verifies() {
        let shards = vec![
            ShardTreeHead {
                shard_id: "game:ashen-realms".to_string(),
                sth: sth("aa", 5),
            },
            ShardTreeHead {
                shard_id: "core".to_string(),
                sth: sth("bb", 10),
            },
            ShardTreeHead {
                shard_id: "app:some-app".to_string(),
                sth: sth("cc", 3),
            },
        ];
        let now = time::OffsetDateTime::from_unix_timestamp(0).unwrap();
        let root = compute_cross_shard_root(shards.clone(), now);

        let ordered = canonical_order(shards.clone());
        let target_index = ordered.iter().position(|s| s.shard_id == "core").unwrap();
        let target = ordered.iter().find(|s| s.shard_id == "core").unwrap();

        let proof = inclusion_proof_for_shard(&shards, "core")
            .expect("shard present")
            .expect("proof generation should succeed");

        let verified = verify_shard_inclusion(
            &root.root_hash,
            root.shard_count,
            target_index,
            &target.shard_id,
            &target.sth,
            &proof,
        )
        .expect("verification should not error");
        assert!(verified, "a real inclusion proof must verify");
    }

    #[test]
    fn inclusion_proof_for_an_absent_shard_is_none() {
        let shards = vec![ShardTreeHead {
            shard_id: "core".to_string(),
            sth: sth("aa", 5),
        }];
        assert!(inclusion_proof_for_shard(&shards, "game:nonexistent").is_none());
    }
}
