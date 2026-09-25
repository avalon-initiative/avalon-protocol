//! Cosigned tree heads — bundles one author [`sth::SignedTreeHead`] with the
//! [`witness::WitnessCosignature`]s that back it, and verifies both
//! together against a verifier's own known list. Wires the pure primitives
//! in `sth.rs`/`witness.rs` into one accept/reject decision; storage and
//! discovery are `crates/chain`/`crates/server`'s job, not this module's
//! (see `docs/projects/backend-server/architecture/witness-cosigning.md`).

use ed25519_dalek::VerifyingKey;
use std::collections::BTreeSet;
use time::OffsetDateTime;

use crate::sth;
use crate::witness;

/// One author-signed tree head plus whatever cosignatures a caller has
/// gathered for it. Not itself signed or hashed — it's a bundle a verifier
/// assembles (from storage, from gossip) before calling
/// [`verify_cosigned_tree_head`], not a wire format with its own identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CosignedTreeHead {
    pub sth: sth::SignedTreeHead,
    pub cosignatures: Vec<witness::WitnessCosignature>,
}

/// The witness ids from `head.cosignatures` that actually count toward
/// majority: bound to this exact STH (every field `witness::witness_signing_message`
/// covers besides the witness's own id/timestamp), individually signature-valid
/// against a key the caller's own `known_list` vouches for, and fresh
/// (`freshness_cutoff <= observed_at <= now` — an old cosignature for a
/// still-current head is not accepted just because it was once fresh, and a
/// future-dated one is rejected the same way a clock-skewed or forged
/// `observed_at` would be). Deduplicated by witness id: a witness cannot
/// count twice toward the same majority by being repeated in the list.
fn valid_fresh_witness_ids(
    head: &CosignedTreeHead,
    known_list: &[(String, VerifyingKey)],
    freshness_cutoff: OffsetDateTime,
    now: OffsetDateTime,
) -> BTreeSet<String> {
    let mut verified = BTreeSet::new();
    for cosig in &head.cosignatures {
        if cosig.tree_size != head.sth.tree_size
            || cosig.root_hash != head.sth.root_hash
            || cosig.network_id != head.sth.network_id
            || cosig.author_created_at != head.sth.created_at
        {
            continue;
        }
        if cosig.observed_at < freshness_cutoff || cosig.observed_at > now {
            continue;
        }
        let Some((_, verifying_key)) = known_list
            .iter()
            .find(|(witness_key_id, _)| *witness_key_id == cosig.witness_key_id)
        else {
            continue;
        };
        if witness::verify_witness_cosignature(verifying_key, cosig) {
            verified.insert(cosig.witness_key_id.clone());
        }
    }
    verified
}

/// Accepts `head` iff its author signature verifies against
/// `author_verifying_key` (exactly [`sth::verify_tree_head`]'s own check)
/// **and** at least `witness::majority_threshold(known_list.len())` of its
/// cosignatures are valid and fresh per [`valid_fresh_witness_ids`].
/// `known_list` is the verifier's own list, handed in by the caller — this
/// function does no discovery or gossip of its own.
///
/// **Degenerate case, by design, not by accident:** a `known_list` of size
/// 0 or 1 skips the cosignature check entirely and returns exactly what
/// `sth::verify_tree_head` would. This is the "single-witness and
/// no-witness networks keep verifying as before" guarantee the rollout
/// depends on — a list of 1 has no witness other than the author itself
/// (self-attestation, `majority_threshold(1) == 1`), so requiring a
/// separately-signed cosignature from that same identity would only
/// duplicate the STH check under a different key type, not add anything a
/// verifier doesn't already have. `verify_cosigned_tree_head_matches_plain_sth_verification_at_and_below_one_known_witness`
/// below asserts this equivalence directly rather than leaving it implicit
/// in the arithmetic.
pub fn verify_cosigned_tree_head(
    author_verifying_key: &VerifyingKey,
    head: &CosignedTreeHead,
    known_list: &[(String, VerifyingKey)],
    freshness_cutoff: OffsetDateTime,
    now: OffsetDateTime,
) -> bool {
    if !sth::verify_tree_head(author_verifying_key, &head.sth) {
        return false;
    }
    if known_list.len() <= 1 {
        return true;
    }
    let verified = valid_fresh_witness_ids(head, known_list, freshness_cutoff, now);
    witness::is_cosigned_by_majority(known_list.len(), verified.len())
}

/// Given two cosigned heads for the same `network_id`/`tree_size` but
/// different `root_hash`, each independently accepted by
/// [`verify_cosigned_tree_head`] against the same `known_list`, returns the
/// witness ids present (valid and fresh) in both heads' cosignature sets —
/// the concrete equivocation proof `witness::majority_threshold`'s
/// intersection guarantee promises must exist. Returns an empty `Vec` for
/// anything that isn't a genuine conflict: different networks, different
/// tree sizes, an identical root (no conflict at all), or either head
/// failing to reach majority on its own (nothing to prove).
pub fn find_equivocating_witnesses(
    author_verifying_key: &VerifyingKey,
    known_list: &[(String, VerifyingKey)],
    freshness_cutoff: OffsetDateTime,
    now: OffsetDateTime,
    head_a: &CosignedTreeHead,
    head_b: &CosignedTreeHead,
) -> Vec<String> {
    if head_a.sth.network_id != head_b.sth.network_id
        || head_a.sth.tree_size != head_b.sth.tree_size
        || head_a.sth.root_hash == head_b.sth.root_hash
    {
        return Vec::new();
    }
    if !verify_cosigned_tree_head(
        author_verifying_key,
        head_a,
        known_list,
        freshness_cutoff,
        now,
    ) || !verify_cosigned_tree_head(
        author_verifying_key,
        head_b,
        known_list,
        freshness_cutoff,
        now,
    ) {
        return Vec::new();
    }

    let witnesses_a = valid_fresh_witness_ids(head_a, known_list, freshness_cutoff, now);
    let witnesses_b = valid_fresh_witness_ids(head_b, known_list, freshness_cutoff, now);
    witnesses_a.intersection(&witnesses_b).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn root_hash_fixture(byte: u8) -> String {
        hex::encode([byte; 32])
    }

    fn base_head(
        author_key: &SigningKey,
        tree_size: i64,
        root_hash: &str,
        network_id: &str,
        created_at: OffsetDateTime,
    ) -> CosignedTreeHead {
        CosignedTreeHead {
            sth: sth::sign_tree_head(
                author_key,
                "settlement-operator-1",
                tree_size,
                root_hash,
                network_id,
                created_at,
            ),
            cosignatures: Vec::new(),
        }
    }

    fn cosign(
        witness_key: &SigningKey,
        witness_key_id: &str,
        head: &CosignedTreeHead,
        observed_at: OffsetDateTime,
    ) -> witness::WitnessCosignature {
        witness::sign_witness_cosignature(
            witness_key,
            witness_key_id,
            head.sth.tree_size,
            &head.sth.root_hash,
            &head.sth.network_id,
            head.sth.created_at,
            observed_at,
        )
    }

    struct Fixture {
        author_key: SigningKey,
        author_verifying_key: VerifyingKey,
        witness_keys: Vec<(String, SigningKey)>,
        known_list: Vec<(String, VerifyingKey)>,
        now: OffsetDateTime,
        freshness_cutoff: OffsetDateTime,
    }

    fn fixture(witness_count: usize) -> Fixture {
        let author_key = SigningKey::generate(&mut rand::rng());
        let witness_keys: Vec<(String, SigningKey)> = (0..witness_count)
            .map(|i| {
                (
                    format!("witness-{i}"),
                    SigningKey::generate(&mut rand::rng()),
                )
            })
            .collect();
        let known_list = witness_keys
            .iter()
            .map(|(id, key)| (id.clone(), key.verifying_key()))
            .collect();
        let now = OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(10_000);
        Fixture {
            author_verifying_key: author_key.verifying_key(),
            author_key,
            witness_keys,
            known_list,
            now,
            freshness_cutoff: now - time::Duration::minutes(10),
        }
    }

    #[test]
    fn accepted_with_a_valid_fresh_majority() {
        let f = fixture(3); // majority_threshold(3) == 2
        let mut head = base_head(
            &f.author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            f.now,
        );
        head.cosignatures.push(cosign(
            &f.witness_keys[0].1,
            &f.witness_keys[0].0,
            &head,
            f.now,
        ));
        head.cosignatures.push(cosign(
            &f.witness_keys[1].1,
            &f.witness_keys[1].0,
            &head,
            f.now,
        ));

        assert!(verify_cosigned_tree_head(
            &f.author_verifying_key,
            &head,
            &f.known_list,
            f.freshness_cutoff,
            f.now
        ));
    }

    #[test]
    fn rejected_below_threshold() {
        let f = fixture(3); // majority_threshold(3) == 2
        let mut head = base_head(
            &f.author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            f.now,
        );
        head.cosignatures.push(cosign(
            &f.witness_keys[0].1,
            &f.witness_keys[0].0,
            &head,
            f.now,
        ));

        assert!(!verify_cosigned_tree_head(
            &f.author_verifying_key,
            &head,
            &f.known_list,
            f.freshness_cutoff,
            f.now
        ));
    }

    #[test]
    fn cosignature_from_a_witness_outside_the_known_list_does_not_count() {
        let f = fixture(3); // majority_threshold(3) == 2
        let mut head = base_head(
            &f.author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            f.now,
        );
        head.cosignatures.push(cosign(
            &f.witness_keys[0].1,
            &f.witness_keys[0].0,
            &head,
            f.now,
        ));
        let stranger_key = SigningKey::generate(&mut rand::rng());
        head.cosignatures
            .push(cosign(&stranger_key, "witness-stranger", &head, f.now));

        // Only one of the two cosignatures came from a key in the known
        // list, so this stays below the threshold of 2 even though the
        // cosignature *count* looks sufficient.
        assert!(!verify_cosigned_tree_head(
            &f.author_verifying_key,
            &head,
            &f.known_list,
            f.freshness_cutoff,
            f.now
        ));
    }

    #[test]
    fn stale_cosignature_does_not_count() {
        let f = fixture(3); // majority_threshold(3) == 2
        let mut head = base_head(
            &f.author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            f.now,
        );
        head.cosignatures.push(cosign(
            &f.witness_keys[0].1,
            &f.witness_keys[0].0,
            &head,
            f.now,
        ));
        let stale_observed_at = f.freshness_cutoff - time::Duration::seconds(1);
        head.cosignatures.push(cosign(
            &f.witness_keys[1].1,
            &f.witness_keys[1].0,
            &head,
            stale_observed_at,
        ));

        assert!(!verify_cosigned_tree_head(
            &f.author_verifying_key,
            &head,
            &f.known_list,
            f.freshness_cutoff,
            f.now
        ));
    }

    #[test]
    fn future_dated_cosignature_does_not_count() {
        let f = fixture(3); // majority_threshold(3) == 2
        let mut head = base_head(
            &f.author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            f.now,
        );
        head.cosignatures.push(cosign(
            &f.witness_keys[0].1,
            &f.witness_keys[0].0,
            &head,
            f.now,
        ));
        let future_observed_at = f.now + time::Duration::seconds(1);
        head.cosignatures.push(cosign(
            &f.witness_keys[1].1,
            &f.witness_keys[1].0,
            &head,
            future_observed_at,
        ));

        assert!(!verify_cosigned_tree_head(
            &f.author_verifying_key,
            &head,
            &f.known_list,
            f.freshness_cutoff,
            f.now
        ));
    }

    #[test]
    fn an_invalid_author_signature_is_rejected_regardless_of_cosignatures() {
        let f = fixture(3);
        let mut head = base_head(
            &f.author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            f.now,
        );
        head.cosignatures.push(cosign(
            &f.witness_keys[0].1,
            &f.witness_keys[0].0,
            &head,
            f.now,
        ));
        head.cosignatures.push(cosign(
            &f.witness_keys[1].1,
            &f.witness_keys[1].0,
            &head,
            f.now,
        ));
        head.sth.root_hash = root_hash_fixture(2); // tampers the signed field

        assert!(!verify_cosigned_tree_head(
            &f.author_verifying_key,
            &head,
            &f.known_list,
            f.freshness_cutoff,
            f.now
        ));
    }

    /// The backward-compatibility guarantee this whole rollout depends on:
    /// with an empty or single-entry known list, `verify_cosigned_tree_head`
    /// must decide exactly what `sth::verify_tree_head` alone would, on both
    /// a genuinely valid and a genuinely invalid STH, cosignatures or not.
    #[test]
    fn verify_cosigned_tree_head_matches_plain_sth_verification_at_and_below_one_known_witness() {
        for witness_count in [0usize, 1] {
            let f = fixture(witness_count);
            let mut valid_head = base_head(
                &f.author_key,
                5,
                &root_hash_fixture(1),
                "avalon-test",
                f.now,
            );
            let mut invalid_head = valid_head.clone();
            invalid_head.sth.root_hash = root_hash_fixture(9);

            // A no-witness network has nothing to attach; a one-witness
            // network might still carry a self-cosignature in the wild —
            // either way it must not change the accept/reject outcome.
            if witness_count == 1 {
                valid_head.cosignatures.push(cosign(
                    &f.witness_keys[0].1,
                    &f.witness_keys[0].0,
                    &valid_head,
                    f.now,
                ));
            }

            assert_eq!(
                verify_cosigned_tree_head(
                    &f.author_verifying_key,
                    &valid_head,
                    &f.known_list,
                    f.freshness_cutoff,
                    f.now
                ),
                sth::verify_tree_head(&f.author_verifying_key, &valid_head.sth),
                "witness_count={witness_count}: diverged on a valid STH"
            );
            assert_eq!(
                verify_cosigned_tree_head(
                    &f.author_verifying_key,
                    &invalid_head,
                    &f.known_list,
                    f.freshness_cutoff,
                    f.now
                ),
                sth::verify_tree_head(&f.author_verifying_key, &invalid_head.sth),
                "witness_count={witness_count}: diverged on a tampered STH"
            );
        }
    }

    #[test]
    fn conflicting_majority_cosigned_heads_share_a_witness() {
        let f = fixture(3); // majority_threshold(3) == 2
        let mut head_a = base_head(
            &f.author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            f.now,
        );
        head_a.cosignatures.push(cosign(
            &f.witness_keys[0].1,
            &f.witness_keys[0].0,
            &head_a,
            f.now,
        ));
        head_a.cosignatures.push(cosign(
            &f.witness_keys[1].1,
            &f.witness_keys[1].0,
            &head_a,
            f.now,
        ));

        // A different root at the same tree_size — witness-1 double-signed
        // (equivocated), witness-2 only ever cosigned head_a.
        let mut head_b = base_head(
            &f.author_key,
            5,
            &root_hash_fixture(2),
            "avalon-test",
            f.now,
        );
        head_b.cosignatures.push(cosign(
            &f.witness_keys[1].1,
            &f.witness_keys[1].0,
            &head_b,
            f.now,
        ));
        head_b.cosignatures.push(cosign(
            &f.witness_keys[2].1,
            &f.witness_keys[2].0,
            &head_b,
            f.now,
        ));

        let equivocators = find_equivocating_witnesses(
            &f.author_verifying_key,
            &f.known_list,
            f.freshness_cutoff,
            f.now,
            &head_a,
            &head_b,
        );
        assert_eq!(equivocators, vec![f.witness_keys[1].0.clone()]);
    }

    #[test]
    fn non_conflicting_heads_report_no_equivocation() {
        let f = fixture(3);
        let head_a = base_head(
            &f.author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            f.now,
        );
        // Same tree_size, same root: not a conflict at all.
        let head_b = head_a.clone();

        assert!(find_equivocating_witnesses(
            &f.author_verifying_key,
            &f.known_list,
            f.freshness_cutoff,
            f.now,
            &head_a,
            &head_b
        )
        .is_empty());
    }

    #[test]
    fn conflicting_heads_that_never_reach_majority_report_no_equivocation() {
        let f = fixture(3); // majority_threshold(3) == 2
        let mut head_a = base_head(
            &f.author_key,
            5,
            &root_hash_fixture(1),
            "avalon-test",
            f.now,
        );
        head_a.cosignatures.push(cosign(
            &f.witness_keys[0].1,
            &f.witness_keys[0].0,
            &head_a,
            f.now,
        ));
        let mut head_b = base_head(
            &f.author_key,
            5,
            &root_hash_fixture(2),
            "avalon-test",
            f.now,
        );
        head_b.cosignatures.push(cosign(
            &f.witness_keys[0].1,
            &f.witness_keys[0].0,
            &head_b,
            f.now,
        ));

        // Both share witness-0, but neither individually reaches the
        // majority threshold of 2 — nothing has actually been proven yet.
        assert!(find_equivocating_witnesses(
            &f.author_verifying_key,
            &f.known_list,
            f.freshness_cutoff,
            f.now,
            &head_a,
            &head_b
        )
        .is_empty());
    }
}
