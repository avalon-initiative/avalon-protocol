//! Witness cosigning primitives — pure logic and signature format, no I/O.
//! Prototype for the #934 design spike; production known-list management,
//! gossip and server wiring are separate tickets (see
//! `avalon-docs/protocol/witness-cosigning.md`).
//!
//! A witness cosignature is a second, independent signature over the exact
//! author-signed [`crate::sth::SignedTreeHead`]: its `(tree_size, root_hash,
//! network_id, created_at)` tuple plus the author's key id and signature, so a
//! cosignature cannot be re-attached to a different signature over the same tuple.
//! A witness that has checked the head extends consistently from what it last saw,
//! with no conflicting root at the same size, attests to that by cosigning it. A
//! head counts as trusted once [`majority_threshold`] distinct witnesses
//! from the verifier's own known list have cosigned it.
//!
//! Both signed messages use the structured layout in [`crate::signing_bytes`]
//! (tags `avalon.witness.cosign` and `avalon.witness.announce`, layout version 1).

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use time::OffsetDateTime;

use crate::ledger_entry::parse_hash;
use crate::signing_bytes::{tags, Builder, Envelope, SigningBytesError};
use crate::sth::SignedTreeHead;

/// Why a witness message could not be built.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WitnessSigningError {
    #[error("root_hash is not 64 lowercase hex characters")]
    InvalidRootHash,
    #[error("author_signature is not 64 bytes of hex")]
    InvalidAuthorSignature,
    #[error("witness key id is not 64 lowercase hex characters")]
    InvalidKeyId,
    #[error(transparent)]
    Layout(#[from] SigningBytesError),
}

/// One witness's cosignature over an author-signed tree head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitnessCosignature {
    pub tree_size: i64,
    pub root_hash: String,
    pub network_id: String,
    /// The author's own STH timestamp, so this cosignature applies only to the
    /// head with that timestamp.
    pub author_created_at: OffsetDateTime,
    /// The author STH's `signing_key_id`, covered by the cosignature.
    pub author_key_id: String,
    /// The author STH's signature (128 hex characters), covered by the cosignature.
    pub author_signature: String,
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
    /// This cosignature's own layout, rules version and extensions; its `hash_algo` is the
    /// author head's Merkle hash algorithm, which names the root it covers.
    pub envelope: Envelope,
}

fn decode_author_signature(hex_text: &str) -> Result<[u8; 64], WitnessSigningError> {
    let bytes = hex::decode(hex_text).map_err(|_| WitnessSigningError::InvalidAuthorSignature)?;
    <[u8; 64]>::try_from(bytes.as_slice()).map_err(|_| WitnessSigningError::InvalidAuthorSignature)
}

/// The exact bytes a witness cosignature covers (tag `avalon.witness.cosign`, header and
/// extensions as in [`crate::signing_bytes`]): `tree_size` `i64`, `network_id` `str`,
/// `author_created_at` `i64` unix seconds, `author_key_id` `str`, author signature (alg `u8` + 64
/// raw), `witness_key_id` `str`, the witness's verifying key (alg `u8` + 32 raw), `observed_at`
/// `i64`, `hash_algo` `u8` of the author's tree, root hash (32 raw). The `signature` field of
/// `cosig` is not read.
pub fn witness_signing_message(
    cosig: &WitnessCosignature,
    witness_key: &[u8; 32],
) -> Result<Vec<u8>, WitnessSigningError> {
    let root = parse_hash("root_hash", &cosig.root_hash)
        .map_err(|_| WitnessSigningError::InvalidRootHash)?;
    let author_signature = decode_author_signature(&cosig.author_signature)?;
    Ok(
        Builder::with_envelope(tags::WITNESS_COSIGN, &cosig.envelope)
            .i64(cosig.tree_size)
            .str(&cosig.network_id)
            .i64(cosig.author_created_at.unix_timestamp())
            .str(&cosig.author_key_id)
            .signature(&author_signature)
            .str(&cosig.witness_key_id)
            .key(witness_key)
            .i64(cosig.observed_at.unix_timestamp())
            .hash_algo(cosig.envelope.hash_algo)
            .hash(&root)
            .finish()?,
    )
}

fn sign_unsigned(
    witness_signing_key: &SigningKey,
    mut cosig: WitnessCosignature,
) -> Result<WitnessCosignature, WitnessSigningError> {
    // Normalizes the author signature to lowercase hex so equal bytes compare equal.
    cosig.author_signature = hex::encode(decode_author_signature(&cosig.author_signature)?);
    let message = witness_signing_message(&cosig, &witness_signing_key.verifying_key().to_bytes())?;
    let signature: Signature = witness_signing_key.sign(&message);
    cosig.signature = hex::encode(signature.to_bytes());
    Ok(cosig)
}

/// Produces `witness_key_id`'s cosignature over the author-signed head `head`.
pub fn sign_witness_cosignature(
    witness_signing_key: &SigningKey,
    witness_key_id: &str,
    head: &SignedTreeHead,
    observed_at: OffsetDateTime,
) -> Result<WitnessCosignature, WitnessSigningError> {
    sign_unsigned(
        witness_signing_key,
        WitnessCosignature {
            tree_size: head.tree_size,
            root_hash: head.root_hash.clone(),
            network_id: head.network_id.clone(),
            author_created_at: head.created_at,
            author_key_id: head.signing_key_id.clone(),
            author_signature: head.signature.clone(),
            witness_key_id: witness_key_id.to_string(),
            observed_at,
            signature: String::new(),
            envelope: Envelope {
                hash_algo: head.envelope.hash_algo,
                ..Envelope::current(tags::WITNESS_COSIGN)
            },
        },
    )
}

/// A new cosignature over the same head as `existing`, with a fresh `observed_at`.
pub fn reattest_witness_cosignature(
    witness_signing_key: &SigningKey,
    witness_key_id: &str,
    existing: &WitnessCosignature,
    observed_at: OffsetDateTime,
) -> Result<WitnessCosignature, WitnessSigningError> {
    sign_unsigned(
        witness_signing_key,
        WitnessCosignature {
            witness_key_id: witness_key_id.to_string(),
            observed_at,
            signature: String::new(),
            ..existing.clone()
        },
    )
}

/// Verifies `cosig`'s signature against `witness_verifying_key` — `false`
/// for a malformed signature as well as an outright-invalid one; never
/// panics on attacker-controlled input.
pub fn verify_witness_cosignature(
    witness_verifying_key: &VerifyingKey,
    cosig: &WitnessCosignature,
) -> bool {
    let Ok(message) = witness_signing_message(cosig, &witness_verifying_key.to_bytes()) else {
        return false;
    };
    let Ok(signature_bytes) = hex::decode(&cosig.signature) else {
        return false;
    };
    let Ok(signature_array) = <[u8; 64]>::try_from(signature_bytes.as_slice()) else {
        return false;
    };
    let signature = Signature::from_bytes(&signature_array);
    witness_verifying_key.verify(&message, &signature).is_ok()
}

/// How far `announced_at` may differ from the verifier's clock for a witness
/// announce proof to be accepted.
pub const WITNESS_ANNOUNCE_MAX_SKEW: time::Duration = time::Duration::hours(1);

/// The exact bytes a witness announce proof covers (tag `avalon.witness.announce`, header and
/// extensions as in [`crate::signing_bytes`]): the base URL as `str`, the witness key (alg `u8` +
/// 32 raw) and `announced_at` as `i64` unix seconds. The audience stays the URL text: a node has no protocol-level
/// id to bind yet. `witness_key_id` must be the lowercase hex of the key.
pub fn witness_announce_message(
    base_url: &str,
    witness_key_id: &str,
    announced_at: OffsetDateTime,
) -> Result<Vec<u8>, WitnessSigningError> {
    let key = parse_hash("witness_key_id", witness_key_id)
        .map_err(|_| WitnessSigningError::InvalidKeyId)?;
    Ok(Builder::new(tags::WITNESS_ANNOUNCE, 1)
        .str(base_url)
        .key(&key)
        .i64(announced_at.unix_timestamp())
        .finish()?)
}

/// Signs a proof that the holder of `witness_signing_key` advertises itself at
/// `base_url`. Lowercase hex.
pub fn sign_witness_announce(
    witness_signing_key: &SigningKey,
    base_url: &str,
    announced_at: OffsetDateTime,
) -> Result<String, WitnessSigningError> {
    let key_id = hex::encode(witness_signing_key.verifying_key().to_bytes());
    let message = witness_announce_message(base_url, &key_id, announced_at)?;
    Ok(hex::encode(witness_signing_key.sign(&message).to_bytes()))
}

/// Verifies a witness announce proof: `witness_key_id` must be a lowercase hex Ed25519
/// verifying key, the signature must verify under it over the announce
/// message, and `announced_at` must be within [`WITNESS_ANNOUNCE_MAX_SKEW`]
/// of `now`. Never panics on attacker-controlled input.
pub fn verify_witness_announce(
    base_url: &str,
    witness_key_id: &str,
    announced_at: OffsetDateTime,
    proof_hex: &str,
    now: OffsetDateTime,
) -> bool {
    if (now - announced_at).abs() > WITNESS_ANNOUNCE_MAX_SKEW {
        return false;
    }
    let Ok(message) = witness_announce_message(base_url, witness_key_id, announced_at) else {
        return false;
    };
    let Ok(key_bytes) = hex::decode(witness_key_id) else {
        return false;
    };
    let Ok(key_array) = <[u8; 32]>::try_from(key_bytes.as_slice()) else {
        return false;
    };
    let Ok(verifying_key) = VerifyingKey::from_bytes(&key_array) else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(proof_hex) else {
        return false;
    };
    let Ok(sig_array) = <[u8; 64]>::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    verifying_key
        .verify(&message, &Signature::from_bytes(&sig_array))
        .is_ok()
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

/// Loads this node's witness-cosigning key: `AVALON_WITNESS_SIGNING_KEY`
/// (a raw 32-byte Ed25519 seed, same hex-encoding convention as
/// [`crate::sth::load_signing_key_from_env`]), with `witness_key_id`
/// defaulting to the hex-encoded verifying key itself — not an arbitrary
/// label — since `known_list`'s slots are bridged to a verifying key by
/// parsing the id as hex key material (`avalon-server`'s
/// `cosign_verify::known_list_verifying_keys`); a witness whose own
/// cosignatures don't carry that same hex id could never be recognized by
/// a verifier bridging the known list this way.
/// `AVALON_WITNESS_SIGNING_KEY_ID` overrides the default for an operator
/// who has a specific reason to.
///
/// **Falls back to this node's settlement-authority key**
/// (`AVALON_SETTLEMENT_SIGNING_KEY`) when no distinct witness key is
/// configured. This is a deliberate choice, not an oversight: reusing one
/// Ed25519 key across the settlement-signing and witness-cosigning domains
/// is cryptographically sound here specifically because
/// [`witness_signing_message`] prepends its own domain tag
/// (`avalon.witness.cosign`), so the exact bytes a witness cosignature
/// covers can never collide with the exact bytes an author's STH signature
/// covers, even though both cover overlapping fields — the same reasoning
/// [`load_verify_key_from_env`](crate::sth::load_verify_key_from_env)
/// already leans on for its own single-operator fallback. A node that only
/// mirrors (never authors a shard) has no settlement key to fall back to
/// and must set `AVALON_WITNESS_SIGNING_KEY` explicitly to cosign anything;
/// a node that already authors a shard cosigns with the same key it
/// already runs, with zero extra configuration, unless it wants stronger
/// domain separation.
pub fn load_witness_signing_key_from_env() -> Result<(SigningKey, String), crate::sth::KeyLoadError>
{
    if let Ok(hex_value) = std::env::var("AVALON_WITNESS_SIGNING_KEY") {
        let seed = crate::sth::parse_key_bytes("AVALON_WITNESS_SIGNING_KEY", &hex_value)?;
        let signing_key = SigningKey::from_bytes(&seed);
        let key_id = std::env::var("AVALON_WITNESS_SIGNING_KEY_ID")
            .unwrap_or_else(|_| hex::encode(signing_key.verifying_key().to_bytes()));
        return Ok((signing_key, key_id));
    }
    let (signing_key, _settlement_key_id) = crate::sth::load_signing_key_from_env()
        .map_err(|_| crate::sth::KeyLoadError::Missing("AVALON_WITNESS_SIGNING_KEY"))?;
    let key_id = hex::encode(signing_key.verifying_key().to_bytes());
    Ok((signing_key, key_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn author_head(author: &SigningKey) -> SignedTreeHead {
        crate::sth::sign_tree_head(
            author,
            "author-key",
            42,
            &"ab".repeat(32),
            "avalon-test",
            OffsetDateTime::UNIX_EPOCH,
        )
        .unwrap()
    }

    fn cosign(witness: &SigningKey, head: &SignedTreeHead) -> WitnessCosignature {
        sign_witness_cosignature(
            witness,
            "witness-1",
            head,
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(5),
        )
        .unwrap()
    }

    #[test]
    fn sign_then_verify_round_trip_succeeds() {
        let witness_key = SigningKey::generate(&mut rand::rng());
        let head = author_head(&SigningKey::generate(&mut rand::rng()));
        let cosig = cosign(&witness_key, &head);
        assert!(verify_witness_cosignature(
            &witness_key.verifying_key(),
            &cosig
        ));
    }

    #[test]
    fn verify_rejects_signature_from_a_different_witness() {
        let witness_key = SigningKey::generate(&mut rand::rng());
        let other_key = SigningKey::generate(&mut rand::rng());
        let head = author_head(&SigningKey::generate(&mut rand::rng()));
        let cosig = cosign(&witness_key, &head);
        assert!(!verify_witness_cosignature(
            &other_key.verifying_key(),
            &cosig
        ));
    }

    #[test]
    fn verify_rejects_a_tampered_field() {
        let witness_key = SigningKey::generate(&mut rand::rng());
        let verifying_key = witness_key.verifying_key();
        let head = author_head(&SigningKey::generate(&mut rand::rng()));
        let cosig = cosign(&witness_key, &head);

        let mut tampered = Vec::new();
        let mut c = cosig.clone();
        c.tree_size += 1;
        tampered.push(c);
        let mut c = cosig.clone();
        c.root_hash = "cd".repeat(32);
        tampered.push(c);
        let mut c = cosig.clone();
        c.network_id = "other".to_string();
        tampered.push(c);
        let mut c = cosig.clone();
        c.author_created_at += time::Duration::seconds(1);
        tampered.push(c);
        let mut c = cosig.clone();
        c.author_key_id = "other-key".to_string();
        tampered.push(c);
        let mut c = cosig.clone();
        c.author_signature = "00".repeat(64);
        tampered.push(c);
        let mut c = cosig.clone();
        c.witness_key_id = "witness-2".to_string();
        tampered.push(c);
        let mut c = cosig.clone();
        c.observed_at += time::Duration::seconds(1);
        tampered.push(c);
        for c in tampered {
            assert!(!verify_witness_cosignature(&verifying_key, &c));
        }
        assert!(verify_witness_cosignature(&verifying_key, &cosig));
    }

    #[test]
    fn cosignature_is_bound_to_the_exact_author_signature() {
        let witness_key = SigningKey::generate(&mut rand::rng());
        let head = author_head(&SigningKey::generate(&mut rand::rng()));
        let other_author = author_head(&SigningKey::generate(&mut rand::rng()));
        assert_ne!(head.signature, other_author.signature);
        let mut cosig = cosign(&witness_key, &head);
        cosig.author_signature = other_author.signature;
        assert!(!verify_witness_cosignature(
            &witness_key.verifying_key(),
            &cosig
        ));
    }

    #[test]
    fn malformed_root_or_author_signature_is_rejected() {
        let witness_key = SigningKey::generate(&mut rand::rng());
        let mut head = author_head(&SigningKey::generate(&mut rand::rng()));
        head.root_hash = "AB".repeat(32);
        assert_eq!(
            sign_witness_cosignature(&witness_key, "w", &head, OffsetDateTime::UNIX_EPOCH)
                .unwrap_err(),
            WitnessSigningError::InvalidRootHash
        );
        let mut head = author_head(&SigningKey::generate(&mut rand::rng()));
        head.signature = "zz".to_string();
        assert_eq!(
            sign_witness_cosignature(&witness_key, "w", &head, OffsetDateTime::UNIX_EPOCH)
                .unwrap_err(),
            WitnessSigningError::InvalidAuthorSignature
        );
    }

    #[test]
    fn witness_announce_round_trips_and_rejects_tampering_and_staleness() {
        let key = SigningKey::generate(&mut rand::rng());
        let id = hex::encode(key.verifying_key().to_bytes());
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::days(100);
        let proof = sign_witness_announce(&key, "http://a.example", now).unwrap();
        assert!(verify_witness_announce(
            "http://a.example",
            &id,
            now,
            &proof,
            now
        ));
        assert!(!verify_witness_announce(
            "http://b.example",
            &id,
            now,
            &proof,
            now
        ));
        assert!(!verify_witness_announce(
            "http://a.example",
            &id,
            now + time::Duration::seconds(1),
            &proof,
            now
        ));
        let other = SigningKey::generate(&mut rand::rng());
        let other_id = hex::encode(other.verifying_key().to_bytes());
        assert!(!verify_witness_announce(
            "http://a.example",
            &other_id,
            now,
            &proof,
            now
        ));
        assert!(!verify_witness_announce(
            "http://a.example",
            &id,
            now,
            "zz",
            now
        ));
        assert!(!verify_witness_announce(
            "http://a.example",
            "not-hex",
            now,
            &proof,
            now
        ));
        assert!(!verify_witness_announce(
            "http://a.example",
            &id.to_uppercase(),
            now,
            &proof,
            now
        ));
        let later = now + WITNESS_ANNOUNCE_MAX_SKEW + time::Duration::seconds(1);
        assert!(!verify_witness_announce(
            "http://a.example",
            &id,
            now,
            &proof,
            later
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

    // SAFETY-of-intent note: process-global env vars, same posture
    // `avalon_server::dht`'s own env-var tests already take — serialized via
    // `crate::test_env::guard`.
    mod witness_key_loading {
        use super::*;

        fn clear_env() {
            unsafe {
                std::env::remove_var("AVALON_WITNESS_SIGNING_KEY");
                std::env::remove_var("AVALON_WITNESS_SIGNING_KEY_ID");
                std::env::remove_var("AVALON_SETTLEMENT_SIGNING_KEY");
            }
        }

        #[test]
        fn loads_a_distinct_witness_key_when_configured() {
            let _env = crate::test_env::guard();
            clear_env();
            let seed = [7u8; 32];
            let expected = SigningKey::from_bytes(&seed);
            unsafe {
                std::env::set_var("AVALON_WITNESS_SIGNING_KEY", hex::encode(seed));
            }

            let (key, key_id) = load_witness_signing_key_from_env().unwrap();
            assert_eq!(key.to_bytes(), expected.to_bytes());
            assert_eq!(key_id, hex::encode(expected.verifying_key().to_bytes()));

            clear_env();
        }

        #[test]
        fn key_id_override_is_respected() {
            let _env = crate::test_env::guard();
            clear_env();
            unsafe {
                std::env::set_var("AVALON_WITNESS_SIGNING_KEY", hex::encode([3u8; 32]));
                std::env::set_var("AVALON_WITNESS_SIGNING_KEY_ID", "custom-witness-id");
            }

            let (_, key_id) = load_witness_signing_key_from_env().unwrap();
            assert_eq!(key_id, "custom-witness-id");

            clear_env();
        }

        #[test]
        fn falls_back_to_the_settlement_key_when_no_witness_key_is_set() {
            let _env = crate::test_env::guard();
            clear_env();
            let seed = [9u8; 32];
            unsafe {
                std::env::set_var("AVALON_SETTLEMENT_SIGNING_KEY", hex::encode(seed));
            }

            let (key, key_id) = load_witness_signing_key_from_env().unwrap();
            let expected = SigningKey::from_bytes(&seed);
            assert_eq!(key.to_bytes(), expected.to_bytes());
            assert_eq!(key_id, hex::encode(expected.verifying_key().to_bytes()));

            clear_env();
        }

        #[test]
        fn errors_when_neither_key_is_configured() {
            let _env = crate::test_env::guard();
            clear_env();
            assert!(load_witness_signing_key_from_env().is_err());
        }
    }
}
