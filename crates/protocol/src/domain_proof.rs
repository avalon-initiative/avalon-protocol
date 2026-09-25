//! Domain proof for a [`crate::shard_identity::NameBindingClaim`] — the
//! layer that decides which of possibly several signed claims for the same
//! `name` is actually authoritative, without any registry or Avalon
//! infrastructure being reachable. Same mechanism the web already uses for
//! domain ownership (a DNS TXT record or a well-known file): the domain's
//! own operator publishes a value, and anyone can fetch and check it.
//!
//! **Proof value: the claim's own signature.** The published value is
//! exactly `avalon-name-proof-v1:<claim.signature>` (lowercase hex, same
//! encoding the claim itself already uses). This is deliberately not a
//! fresh hash of the key: the claim's signature already commits to
//! `(self_certifying_id, public_key, name, created_at)` as a single
//! unforgeable unit, so publishing it as the proof value ties the domain
//! directly to *that exact claim* — a different key, a different name, or
//! even the same key claiming the same name at a different `created_at`
//! produces a different signature and therefore a different required proof
//! value. There is nothing to replay: presenting this proof for any claim
//! other than the one it was generated for fails [`verify_domain_proof`]
//! before the domain check even runs, because [`verify_name_binding_claim`]
//! already rejects a tampered claim.
//!
//! **Where the value is published** (the fetch itself is I/O and belongs to
//! whatever crate does the fetching — `crates/server` today):
//! - Well-known file: [`WELL_KNOWN_PATH`] on the claimed domain
//!   (`https://<name><WELL_KNOWN_PATH>`), containing the proof value and
//!   nothing else (surrounding whitespace ignored).
//! - DNS TXT record: [`dns_txt_record_name`] on the claimed domain,
//!   containing the same proof value.
//!
//! Either is sufficient; a verifier only needs one to check out.

use time::OffsetDateTime;

use crate::shard_identity::{verify_name_binding_claim, NameBindingClaim};

/// Prefix every published proof value carries, so a value copy-pasted from
/// an unrelated context (or a future, differently-shaped proof format) is
/// never mistaken for a valid one.
pub const DOMAIN_PROOF_PREFIX: &str = "avalon-name-proof-v1:";

/// Well-known path a claimed domain publishes its proof file at.
pub const WELL_KNOWN_PATH: &str = "/.well-known/avalon-name-proof";

/// DNS TXT record name a claimed domain publishes its proof at, e.g.
/// `_avalon-challenge.wow-demo.example`.
pub fn dns_txt_record_name(name: &str) -> String {
    format!("_avalon-challenge.{name}")
}

/// The exact proof value `claim`'s domain must publish (at [`WELL_KNOWN_PATH`]
/// or [`dns_txt_record_name`]) for the claim to be provable. Pure string
/// derivation — no I/O, no claim validity check.
pub fn expected_domain_proof(claim: &NameBindingClaim) -> String {
    format!("{DOMAIN_PROOF_PREFIX}{}", claim.signature)
}

/// Verifies `claim` end to end AND that `fetched_proof` (whatever fetched it
/// — well-known file or DNS TXT record) is exactly the value that claim's
/// domain must publish. `false` for a claim that doesn't verify on its own,
/// or a proof value that doesn't match — including a proof correctly
/// published for a *different* claim (different key, name, or timestamp),
/// which can never collide with this one's expected value.
pub fn verify_domain_proof(claim: &NameBindingClaim, fetched_proof: &str) -> bool {
    verify_name_binding_claim(claim) && fetched_proof.trim() == expected_domain_proof(claim)
}

/// Deterministic tiebreak between two independently domain-proven claims for
/// the *same* `name` (callers only invoke this once both have already
/// passed [`verify_domain_proof`] — this function does not itself check
/// proofs). Earliest `created_at` wins: the domain's live DNS/HTTP state is
/// exactly the kind of thing that can flap, be cached differently per
/// verifier, or be fetched at different times by different nodes, so using
/// "whichever proof was fetched most recently" would let two honest
/// verifiers reach different answers for the same pair of claims depending
/// on network timing alone. `created_at` is a fixed, signed field inside
/// the claim itself — every verifier who has both claims computes the same
/// answer regardless of when or how they fetched either proof. Ties broken
/// on `created_at` down to the second (an attacker forging a race would
/// still need to win a coin flip): the lexicographically smaller
/// `public_key` wins, an arbitrary but fixed and universally reproducible
/// total order.
pub fn contested_name_winner<'a>(
    a: &'a NameBindingClaim,
    b: &'a NameBindingClaim,
) -> &'a NameBindingClaim {
    match a.created_at.cmp(&b.created_at) {
        std::cmp::Ordering::Less => a,
        std::cmp::Ordering::Greater => b,
        std::cmp::Ordering::Equal => {
            if a.public_key <= b.public_key {
                a
            } else {
                b
            }
        }
    }
}

/// A claim's domain proof is only ever checked as of when it was fetched —
/// `created_at` here is the claim's own timestamp, kept alongside for
/// callers that want to reject a claim signed implausibly far in the past
/// or future without pulling in a full expiry mechanism (none is imposed by
/// this module itself).
pub fn claim_age(claim: &NameBindingClaim, now: OffsetDateTime) -> time::Duration {
    now - claim.created_at
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    use crate::shard_identity::sign_name_binding_claim;

    fn claim_at(name: &str, created_at: OffsetDateTime) -> NameBindingClaim {
        let key = SigningKey::generate(&mut rand::rng());
        sign_name_binding_claim(&key, name, created_at)
    }

    #[test]
    fn proof_format_round_trips() {
        let claim = claim_at("wow-demo.example", OffsetDateTime::UNIX_EPOCH);
        let proof = expected_domain_proof(&claim);
        assert!(proof.starts_with(DOMAIN_PROOF_PREFIX));
        assert!(verify_domain_proof(&claim, &proof));
        // Leading/trailing whitespace (as a TXT record or file may carry)
        // does not break verification.
        assert!(verify_domain_proof(&claim, &format!("  {proof}\n")));
    }

    #[test]
    fn forged_proof_is_rejected() {
        let claim = claim_at("wow-demo.example", OffsetDateTime::UNIX_EPOCH);
        assert!(!verify_domain_proof(
            &claim,
            "avalon-name-proof-v1:deadbeef"
        ));
        assert!(!verify_domain_proof(&claim, ""));
    }

    #[test]
    fn proof_for_a_different_claim_does_not_carry_over() {
        let claim_a = claim_at("wow-demo.example", OffsetDateTime::UNIX_EPOCH);
        let claim_b = claim_at(
            "wow-demo.example",
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(1),
        );
        let proof_a = expected_domain_proof(&claim_a);
        // A proof correctly published for claim_a's exact signature never
        // verifies against claim_b, even though both name the same domain.
        assert!(!verify_domain_proof(&claim_b, &proof_a));
    }

    #[test]
    fn proof_is_rejected_for_a_claim_that_does_not_verify_on_its_own() {
        let mut claim = claim_at("wow-demo.example", OffsetDateTime::UNIX_EPOCH);
        let proof = expected_domain_proof(&claim);
        claim.name = "not-wow-demo.example".to_string();
        assert!(!verify_domain_proof(&claim, &proof));
    }

    #[test]
    fn earliest_created_at_wins_the_contest() {
        let earlier = claim_at("wow-demo.example", OffsetDateTime::UNIX_EPOCH);
        let later = claim_at(
            "wow-demo.example",
            OffsetDateTime::UNIX_EPOCH + time::Duration::seconds(60),
        );
        assert_eq!(contested_name_winner(&earlier, &later), &earlier);
        assert_eq!(contested_name_winner(&later, &earlier), &earlier);
    }

    #[test]
    fn identical_timestamps_break_the_tie_on_public_key() {
        let a = claim_at("wow-demo.example", OffsetDateTime::UNIX_EPOCH);
        let b = claim_at("wow-demo.example", OffsetDateTime::UNIX_EPOCH);
        let expected = if a.public_key <= b.public_key { &a } else { &b };
        assert_eq!(contested_name_winner(&a, &b), expected);
        assert_eq!(contested_name_winner(&b, &a), expected);
    }

    #[test]
    fn dns_record_name_is_scoped_under_a_fixed_label() {
        assert_eq!(
            dns_txt_record_name("wow-demo.example"),
            "_avalon-challenge.wow-demo.example"
        );
    }
}
