//! Witness cosigning primitives — pure logic and signature format, no I/O.
//! Prototype for the #934 design spike; production known-list management,
//! gossip and server wiring are separate tickets (see
//! `docs/projects/backend-server/architecture/witness-cosigning.md`).
//!
//! A witness cosignature is a second, independent signature over the exact
//! same `(tree_size, root_hash, network_id, created_at)` tuple an author's
//! own [`crate::sth::SignedTreeHead`] already signs — a witness that has
//! checked the head extends consistently from what it last saw, with no
//! conflicting root at the same size, attests to that by cosigning it. A
//! head counts as trusted once [`majority_threshold`] distinct witnesses
//! from the verifier's own known list have cosigned it.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use time::OffsetDateTime;

/// One witness's cosignature over an author-signed tree head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessCosignature {
    pub tree_size: i64,
    pub root_hash: String,
    pub network_id: String,
    /// The author's own STH timestamp — binds this cosignature to that
    /// exact STH, not just to the (tree_size, root_hash, network_id) triple
    /// (an author could in principle re-sign the same triple at a different
    /// time; this keeps a cosignature from applying to a head it never saw).
    pub author_created_at: OffsetDateTime,
    /// Hex-encoded Ed25519 public key identifying which witness this is —
    /// the known list is keyed by this, not by network address.
    pub witness_key_id: String,
    /// When this witness produced the cosignature — distinct from
    /// `author_created_at`, and what freshness-window checks use: an old
    /// cosignature for a since-superseded head stays valid forever (the
    /// head it attests to never stops being a real, once-true checkpoint),
    /// but a witness that hasn't produced a *fresh* cosignature for the
    /// network's current head within the freshness window is a stale slot,
    /// not a broken guarantee.
    pub observed_at: OffsetDateTime,
    /// Lowercase hex-encoded Ed25519 signature (64 bytes).
    pub signature: String,
}

/// The exact bytes a witness cosignature covers — same length-prefixing
/// discipline as `crate::sth::signing_message`, with its own domain tag so
/// a witness cosignature can never be mistaken for an author's STH
/// signature even though both cover overlapping fields.
pub fn witness_signing_message(
    tree_size: i64,
    root_hash_hex: &str,
    network_id: &str,
    author_created_at: OffsetDateTime,
    witness_key_id: &str,
    observed_at: OffsetDateTime,
) -> Vec<u8> {
    let mut message = Vec::new();
    message.extend_from_slice(b"avalon-witness-cosign-v1");
    message.extend_from_slice(&tree_size.to_be_bytes());
    message.extend_from_slice(&(root_hash_hex.len() as u32).to_be_bytes());
    message.extend_from_slice(root_hash_hex.as_bytes());
    message.extend_from_slice(&(network_id.len() as u32).to_be_bytes());
    message.extend_from_slice(network_id.as_bytes());
    message.extend_from_slice(&author_created_at.unix_timestamp().to_be_bytes());
    message.extend_from_slice(&(witness_key_id.len() as u32).to_be_bytes());
    message.extend_from_slice(witness_key_id.as_bytes());
    message.extend_from_slice(&observed_at.unix_timestamp().to_be_bytes());
    message
}

/// Produces `witness_key_id`'s cosignature over an author-signed head.
#[allow(clippy::too_many_arguments)]
pub fn sign_witness_cosignature(
    witness_signing_key: &SigningKey,
    witness_key_id: &str,
    tree_size: i64,
    root_hash_hex: &str,
    network_id: &str,
    author_created_at: OffsetDateTime,
    observed_at: OffsetDateTime,
) -> WitnessCosignature {
    let message = witness_signing_message(
        tree_size,
        root_hash_hex,
        network_id,
        author_created_at,
        witness_key_id,
        observed_at,
    );
    let signature: Signature = witness_signing_key.sign(&message);
    WitnessCosignature {
        tree_size,
        root_hash: root_hash_hex.to_string(),
        network_id: network_id.to_string(),
        author_created_at,
        witness_key_id: witness_key_id.to_string(),
        observed_at,
        signature: hex::encode(signature.to_bytes()),
    }
}

/// Verifies `cosig`'s signature against `witness_verifying_key` — `false`
/// for a malformed signature as well as an outright-invalid one; never
/// panics on attacker-controlled input.
pub fn verify_witness_cosignature(
    witness_verifying_key: &VerifyingKey,
    cosig: &WitnessCosignature,
) -> bool {
    let message = witness_signing_message(
        cosig.tree_size,
        &cosig.root_hash,
        &cosig.network_id,
        cosig.author_created_at,
        &cosig.witness_key_id,
        cosig.observed_at,
    );
    let Ok(signature_bytes) = hex::decode(&cosig.signature) else {
        return false;
    };
    let Ok(signature_array) = <[u8; 64]>::try_from(signature_bytes.as_slice()) else {
        return false;
    };
    let signature = Signature::from_bytes(&signature_array);
    witness_verifying_key.verify(&message, &signature).is_ok()
}

/// The smallest X such that any two size-X subsets of a `list_size`-element
/// known list are guaranteed to share at least one member: `list_size / 2 +
/// 1`. This is the entire fork-detection guarantee — two heads each cosigned
/// by a majority can only both be majority-cosigned if some witness
/// cosigned both, which is either the same head (no fork) or one witness
/// having broken its own no-double-cosign rule (a provable equivocation).
/// `0` for an empty list — no witnesses configured yet, nothing to require.
pub fn majority_threshold(list_size: usize) -> usize {
    if list_size == 0 {
        0
    } else {
        list_size / 2 + 1
    }
}

/// Whether `distinct_witnesses` cosignatures (already deduplicated by
/// witness key and each already signature-verified by the caller) meet
/// `list_size`'s majority threshold.
pub fn is_cosigned_by_majority(list_size: usize, distinct_witnesses: usize) -> bool {
    distinct_witnesses >= majority_threshold(list_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_hash_fixture() -> String {
        "ab".repeat(32)
    }

    #[test]
    fn sign_then_verify_round_trip_succeeds() {
        let witness_key = SigningKey::generate(&mut rand::rng());
        let verifying_key = witness_key.verifying_key();

        let cosig = sign_witness_cosignature(
            &witness_key,
            "witness-1",
            42,
            &root_hash_fixture(),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(5),
        );

        assert!(verify_witness_cosignature(&verifying_key, &cosig));
    }

    #[test]
    fn verify_rejects_signature_from_a_different_witness() {
        let witness_key = SigningKey::generate(&mut rand::rng());
        let other_key = SigningKey::generate(&mut rand::rng());

        let cosig = sign_witness_cosignature(
            &witness_key,
            "witness-1",
            1,
            &root_hash_fixture(),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
            OffsetDateTime::UNIX_EPOCH,
        );

        assert!(!verify_witness_cosignature(
            &other_key.verifying_key(),
            &cosig
        ));
    }

    #[test]
    fn verify_rejects_a_tampered_field() {
        let witness_key = SigningKey::generate(&mut rand::rng());
        let verifying_key = witness_key.verifying_key();
        let cosig = sign_witness_cosignature(
            &witness_key,
            "witness-1",
            7,
            &root_hash_fixture(),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
            OffsetDateTime::UNIX_EPOCH,
        );

        let mut tampered_size = cosig.clone();
        tampered_size.tree_size += 1;
        assert!(!verify_witness_cosignature(&verifying_key, &tampered_size));

        let mut tampered_author_time = cosig.clone();
        tampered_author_time.author_created_at += time::Duration::seconds(1);
        assert!(!verify_witness_cosignature(
            &verifying_key,
            &tampered_author_time
        ));

        let mut tampered_witness_id = cosig.clone();
        tampered_witness_id.witness_key_id = "witness-2".to_string();
        assert!(!verify_witness_cosignature(
            &verifying_key,
            &tampered_witness_id
        ));

        let mut tampered_observed_at = cosig.clone();
        tampered_observed_at.observed_at += time::Duration::seconds(1);
        assert!(!verify_witness_cosignature(
            &verifying_key,
            &tampered_observed_at
        ));
    }

    #[test]
    fn majority_threshold_matches_hand_computed_values() {
        // A lone node is its own witness: majority of one is one, the same
        // rule degenerating to today's single-signer case, not a special
        // case in code.
        assert_eq!(majority_threshold(0), 0);
        assert_eq!(majority_threshold(1), 1);
        assert_eq!(majority_threshold(2), 2);
        assert_eq!(majority_threshold(3), 2);
        assert_eq!(majority_threshold(9), 5);
        // The default known-list cap (Y=10): majority is 6.
        assert_eq!(majority_threshold(10), 6);
        assert_eq!(majority_threshold(11), 6);
    }

    /// The actual fork-detection guarantee, checked directly rather than by
    /// sampling: `majority_threshold` is defined as `list_size / 2 + 1`,
    /// which makes `2 * threshold > list_size` for every list size — by the
    /// pigeonhole principle, two subsets of `list_size` each of size
    /// `threshold` cannot be disjoint, since their sizes alone already sum
    /// to more than the list. Two conflicting heads at the same tree_size,
    /// each independently majority-cosigned, are therefore structurally
    /// impossible unless some witness cosigned both — this is what makes an
    /// equivocation provable rather than merely likely, at every list size,
    /// not just the ones small enough to double-check by brute force below.
    #[test]
    fn majority_threshold_always_exceeds_half_the_list() {
        for list_size in 1..=10_000usize {
            let threshold = majority_threshold(list_size);
            assert!(
                2 * threshold > list_size,
                "list_size={list_size} threshold={threshold}: two disjoint size-threshold \
                 subsets would fit side by side, breaking the fork-detection guarantee"
            );
        }
    }

    /// The same property, double-checked by brute-force enumeration of
    /// every subset pair for small known-list sizes (large enough to cover
    /// the interesting even/odd-size cases, small enough that enumerating
    /// every pair of size-`threshold` subsets is instant) — a concrete
    /// demonstration behind the arithmetic argument above, not just algebra.
    #[test]
    fn any_two_majority_subsets_of_a_small_known_list_always_intersect() {
        for list_size in 1..=12usize {
            let threshold = majority_threshold(list_size);
            let all_subsets_of_size_threshold: Vec<u32> = (0u32..(1 << list_size))
                .filter(|mask| mask.count_ones() as usize == threshold)
                .collect();

            for &a in &all_subsets_of_size_threshold {
                for &b in &all_subsets_of_size_threshold {
                    assert_ne!(
                        a & b,
                        0,
                        "list_size={list_size} threshold={threshold}: subsets {a:#b} and {b:#b} share no witness"
                    );
                }
            }
        }
    }

    /// The same property at known-list sizes too large to enumerate every
    /// subset pair exhaustively (the default cap of 10 and the sizes a
    /// network might legitimately run above it), checked instead by
    /// generating many random majority-sized subset pairs — a randomized
    /// spot-check backing the same guarantee the exhaustive test proves
    /// completely for smaller sizes.
    #[test]
    fn majority_subsets_still_intersect_at_and_above_the_default_known_list_size() {
        use rand::seq::SliceRandom;

        for list_size in [10usize, 25, 50, 100] {
            let threshold = majority_threshold(list_size);
            let witnesses: Vec<usize> = (0..list_size).collect();
            let mut rng = rand::rng();

            for _ in 0..2_000 {
                let mut shuffled_a = witnesses.clone();
                shuffled_a.shuffle(&mut rng);
                let subset_a: std::collections::BTreeSet<usize> =
                    shuffled_a.into_iter().take(threshold).collect();

                let mut shuffled_b = witnesses.clone();
                shuffled_b.shuffle(&mut rng);
                let subset_b: std::collections::BTreeSet<usize> =
                    shuffled_b.into_iter().take(threshold).collect();

                assert!(
                    subset_a.intersection(&subset_b).next().is_some(),
                    "list_size={list_size} threshold={threshold}: subsets {subset_a:?} and \
                     {subset_b:?} share no witness"
                );
            }
        }
    }

    #[test]
    fn is_cosigned_by_majority_matches_the_threshold() {
        assert!(!is_cosigned_by_majority(10, 5));
        assert!(is_cosigned_by_majority(10, 6));
        assert!(is_cosigned_by_majority(10, 10));
        assert!(is_cosigned_by_majority(1, 1));
        assert!(!is_cosigned_by_majority(1, 0));
    }
}
