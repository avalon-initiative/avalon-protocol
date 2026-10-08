//! Achievements as verifiable claims (attestations), not shared database rows.
//!
//! Avalon records that an issuer made a claim about a user. It never
//! dictates what a receiving integrator does with that claim — see
//! `avalon-docs/architecture/design-proposal.md` §8–9, `avalon-docs/protocol/achievements-and-attestations.md`, and the trust
//! model in `avalon-docs/protocol/trust-model.md`.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use sha2::{Digest, Sha256};

use crate::signing_bytes::{tags, Builder, DomainTag, HashAlgo};

use crate::ids::{AttestationId, GlobalId, IdentityId, IntegratorId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AchievementDefinition {
    pub id: GlobalId,
    pub issuer: Issuer,
    pub name: String,
    pub description: String,
    /// An issuer-declared schema reference (e.g. an integrator-event-result schema)
    /// so a consumer can recognize a claim's shape independently of the
    /// issuer's own naming for it. Optional: not every definition needs one.
    pub schema: Option<GlobalId>,
    /// Bumped on every `achievement.definition_updated`; the definition's
    /// `id` never changes, so this is what lets a consumer notice a
    /// definition evolved (see `docs/projects/backend-server/architecture/achievements-and-attestations.md`).
    pub version: u32,
}

/// Whoever is entitled to issue attestations. `Game`/`App`/`Service` mirror
/// `IntegratorCategory` — additive sibling
/// variants sharing `IntegratorId`'s id space, each minting its own `GlobalId`
/// namespace prefix (`game:`/`app:`/`service:`) for events they author.
/// `Issuer::Game` itself is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Issuer {
    Game(IntegratorId),
    App(IntegratorId),
    Service(IntegratorId),
}

impl Issuer {
    /// The `GlobalId` namespace prefix this issuer's authored events use —
    /// `"game"`/`"app"`/`"service"`, matching `IntegratorCategory::as_str()`.
    pub fn namespace(&self) -> &'static str {
        match self {
            Issuer::Game(_) => "game",
            Issuer::App(_) => "app",
            Issuer::Service(_) => "service",
        }
    }

    /// The claim-vocabulary word this issuer's category uses:
    /// `"achievement"` for `Game`, `"milestone"` for `App`/
    /// `Service` — matches `avalon_protocol::integrators::IntegratorCategory::claim_kind()`
    /// exactly (kept as its own method here rather than converting through
    /// `IntegratorCategory`, since `Issuer` is what this module's own types
    /// are already built around).
    pub fn claim_kind(&self) -> &'static str {
        match self {
            Issuer::Game(_) => "achievement",
            Issuer::App(_) | Issuer::Service(_) => "milestone",
        }
    }

    pub fn id(&self) -> IntegratorId {
        match self {
            Issuer::Game(id) | Issuer::App(id) | Issuer::Service(id) => *id,
        }
    }
}

/// A detached Ed25519 signature over an attestation's canonical bytes,
/// produced by one of the issuer's own keys
/// (`avalon_protocol::integrators::IssuerKey`) — never by the node operator.
/// `key_id` names which of the issuer's (possibly several) keys signed it,
/// so verification can resolve that exact key's point-in-time validity at
/// `issued_at` (`resolve_valid_signing_key`) rather than assuming the
/// issuer's current key set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub key_id: String,
    pub algorithm: String,
    pub bytes: Vec<u8>,
}

/// A signed, verifiable claim: `issuer` claims `subject` earned `achievement`.
///
/// The receiving integrator decides independently whether it trusts `issuer` and
/// what the claim means to it — see `TrustRelationship` and `Proposal.md` §9.
///
/// **No `revoked_at` here**: a mutable status field on durable protocol
/// history is exactly what this protocol's revocation model forbids.
/// Revocation is its own append-only entry — not built yet, and
/// deliberately not improvised here as a side effect of shipping issuance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AchievementAttestation {
    pub id: AttestationId,
    pub issuer: Issuer,
    pub subject: IdentityId,
    pub achievement: GlobalId,
    pub issued_at: OffsetDateTime,
    /// Verified against the issuer's own key
    /// (`avalon_chain::attestations::verify_authenticity`) before this
    /// attestation is ever stored — see [`attestation_signing_bytes`] for
    /// exactly what's signed.
    pub proof: Signature,
}

/// How far a signed `issued_at` may sit from the verifying node's clock before issuance is refused.
pub const ISSUED_AT_MAX_SKEW_SECS: i64 = 300;

/// What every attestation signature binds besides its own fields: the network, the issuer and the
/// exact issuer key that signed. An attestation signed for one network never verifies on another.
#[derive(Debug, Clone, Copy)]
pub struct AttestationSigner<'a> {
    pub network_id: &'a str,
    /// `"achievement"` or `"milestone"` ([`Issuer::claim_kind`]).
    pub claim_kind: &'a str,
    /// `"<namespace>:<slug>"`.
    pub issuer_ref: &'a str,
    pub signing_key_id: Uuid,
}

impl AttestationSigner<'_> {
    fn builder(&self, tag: DomainTag) -> Builder {
        Builder::new(tag, 1)
            .str(self.network_id)
            .str(self.claim_kind)
            .str(self.issuer_ref)
            .uuid(self.signing_key_id)
    }
}

/// An `issued_at` as signed: unix microseconds, the precision the database stores.
pub fn issued_at_micros(issued_at: OffsetDateTime) -> i64 {
    (issued_at.unix_timestamp_nanos() / 1000) as i64
}

/// Bytes an issuer key signs to issue one attestation: tag `avalon.attestation.issue`, layout
/// version 1, then `network_id`, `claim_kind`, `issuer_ref` str, `signing_key_id` uuid, `subject`
/// 32 raw bytes, `achievement` str (the claim's full [`GlobalId`]) and `issued_at` i64 microseconds.
pub fn attestation_signing_bytes(
    signer: &AttestationSigner<'_>,
    subject: IdentityId,
    achievement: &str,
    issued_at_micros: i64,
) -> Vec<u8> {
    signer
        .builder(tags::ATTESTATION_ISSUE)
        .fixed(subject.as_bytes())
        .str(achievement)
        .i64(issued_at_micros)
        .finish()
        .expect("attestation fields fit a u32 length")
}

/// Bytes an issuer key signs for a bulk issuance (N ordinary attestations under one signature):
/// tag `avalon.attestation.bulk_issue`, layout version 1, the same header fields as a single
/// issuance, `subject` and `issued_at`, then `count` u32 and each ordered `achievement` str.
pub fn bulk_attestation_signing_bytes(
    signer: &AttestationSigner<'_>,
    subject: IdentityId,
    achievements: &[String],
    issued_at_micros: i64,
) -> Vec<u8> {
    let builder = signer
        .builder(tags::ATTESTATION_BULK_ISSUE)
        .fixed(subject.as_bytes())
        .i64(issued_at_micros)
        .u32(u32::try_from(achievements.len()).expect("claim count fits a u32"));
    achievements
        .iter()
        .fold(builder, |b, achievement| b.str(achievement))
        .finish()
        .expect("attestation fields fit a u32 length")
}

/// SHA-256 of a revocation reason's UTF-8 bytes, the form the revocation signature covers.
pub fn reason_hash(reason: &str) -> [u8; 32] {
    Sha256::digest(reason.as_bytes()).into()
}

/// Bytes an issuer key signs to revoke an attestation (#85: an appended entry, never a mutation):
/// tag `avalon.attestation.revoke`, layout version 1, the shared header fields, then
/// `attestation_id` uuid, `reason_code` str, then `hash_algo` and the 32-byte SHA-256 of the
/// UTF-8 `reason` ([`reason_hash`]), so the text can be redacted while the proof stays valid. The
/// reason code is a plain length-prefixed string, so an unrecognised code has one encoding.
pub fn revocation_signing_bytes(
    signer: &AttestationSigner<'_>,
    attestation_id: AttestationId,
    reason_code: &str,
    reason_hash: &[u8; 32],
) -> Vec<u8> {
    signer
        .builder(tags::ATTESTATION_REVOKE)
        .uuid(attestation_id.0)
        .str(reason_code)
        .hash_algo(HashAlgo::Sha256)
        .hash(reason_hash)
        .finish()
        .expect("revocation fields fit a u32 length")
}

#[cfg(test)]
mod issuer_tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn game_issuer_namespace_is_unchanged() {
        let issuer = Issuer::Game(IntegratorId(Uuid::new_v4()));
        assert_eq!(issuer.namespace(), "game");
    }

    #[test]
    fn app_issuer_mints_the_app_namespace() {
        let id = IntegratorId(Uuid::new_v4());
        let issuer = Issuer::App(id);
        assert_eq!(issuer.namespace(), "app");
        assert_eq!(issuer.id(), id);
    }

    #[test]
    fn service_issuer_mints_the_service_namespace() {
        let id = IntegratorId(Uuid::new_v4());
        let issuer = Issuer::Service(id);
        assert_eq!(issuer.namespace(), "service");
        assert_eq!(issuer.id(), id);
    }

    #[test]
    fn issuer_variants_produce_distinct_global_ids_for_the_same_id() {
        let id = IntegratorId(Uuid::new_v4());
        let owner = id.0.to_string();

        let integrator_ref = GlobalId::new(Issuer::Game(id).namespace(), &owner, "self", "x");
        let app_ref = GlobalId::new(Issuer::App(id).namespace(), &owner, "self", "x");
        let service_ref = GlobalId::new(Issuer::Service(id).namespace(), &owner, "self", "x");

        assert_eq!(integrator_ref.as_str(), format!("game:{owner}:self:x"));
        assert_eq!(app_ref.as_str(), format!("app:{owner}:self:x"));
        assert_eq!(service_ref.as_str(), format!("service:{owner}:self:x"));
        assert_ne!(integrator_ref, app_ref);
        assert_ne!(app_ref, service_ref);
    }

    #[test]
    fn claim_kind_matches_the_decided_vocabulary_split() {
        let id = IntegratorId(Uuid::new_v4());
        assert_eq!(Issuer::Game(id).claim_kind(), "achievement");
        assert_eq!(Issuer::App(id).claim_kind(), "milestone");
        assert_eq!(Issuer::Service(id).claim_kind(), "milestone");
    }

    const T: i64 = 1_700_000_000_000_000;

    fn signer<'a>(claim_kind: &'a str, issuer_ref: &'a str, key: u128) -> AttestationSigner<'a> {
        AttestationSigner {
            network_id: "net",
            claim_kind,
            issuer_ref,
            signing_key_id: Uuid::from_u128(key),
        }
    }

    #[test]
    fn issue_bytes_have_the_documented_layout() {
        let subject = IdentityId::random_for_tests();
        let bytes =
            attestation_signing_bytes(&signer("achievement", "game:a", 5), subject, "g:x", T);
        let mut expected = b"avalon.attestation.issue".to_vec();
        expected.extend_from_slice(&[0, 1, 0, 0, 0, 1]);
        for text in ["net", "achievement", "game:a"] {
            expected.extend_from_slice(&(text.len() as u32).to_be_bytes());
            expected.extend_from_slice(text.as_bytes());
        }
        expected.extend_from_slice(&Uuid::from_u128(5).into_bytes());
        expected.extend_from_slice(subject.as_bytes());
        expected.extend_from_slice(&[0, 0, 0, 3]);
        expected.extend_from_slice(b"g:x");
        expected.extend_from_slice(&T.to_be_bytes());
        expected.extend_from_slice(&[0, 0]);
        assert_eq!(bytes, expected);
    }

    #[test]
    fn issue_bytes_cover_every_field() {
        let subject = IdentityId::random_for_tests();
        let base = attestation_signing_bytes(&signer("achievement", "game:a", 5), subject, "x", T);
        let variants = [
            attestation_signing_bytes(&signer("milestone", "game:a", 5), subject, "x", T),
            attestation_signing_bytes(&signer("achievement", "game:b", 5), subject, "x", T),
            attestation_signing_bytes(&signer("achievement", "game:a", 6), subject, "x", T),
            attestation_signing_bytes(
                &signer("achievement", "game:a", 5),
                IdentityId::random_for_tests(),
                "x",
                T,
            ),
            attestation_signing_bytes(&signer("achievement", "game:a", 5), subject, "y", T),
            attestation_signing_bytes(&signer("achievement", "game:a", 5), subject, "x", T + 1),
        ];
        for variant in variants {
            assert_ne!(base, variant);
        }
    }

    #[test]
    fn every_layout_binds_the_network() {
        let subject = IdentityId::random_for_tests();
        let (a, b) = (
            signer("achievement", "game:a", 5),
            signer("achievement", "game:a", 5),
        );
        let b = AttestationSigner {
            network_id: "other",
            ..b
        };
        let id = AttestationId(Uuid::from_u128(9));
        let list = ["x".to_string()];
        assert_ne!(
            attestation_signing_bytes(&a, subject, "x", T),
            attestation_signing_bytes(&b, subject, "x", T)
        );
        assert_ne!(
            bulk_attestation_signing_bytes(&a, subject, &list, T),
            bulk_attestation_signing_bytes(&b, subject, &list, T)
        );
        assert_ne!(
            revocation_signing_bytes(&a, id, "c", &reason_hash("r")),
            revocation_signing_bytes(&b, id, "c", &reason_hash("r"))
        );
    }

    #[test]
    fn colon_bearing_fields_cannot_shift_boundaries() {
        let subject = IdentityId::random_for_tests();
        let a = attestation_signing_bytes(&signer("milestone", "app:w:x", 1), subject, "y", T);
        let b = attestation_signing_bytes(&signer("milestone", "app:w", 1), subject, "x:y", T);
        assert_ne!(a, b);
    }

    #[test]
    fn bulk_bytes_cover_order_split_boundaries_and_time() {
        let subject = IdentityId::random_for_tests();
        let s = signer("achievement", "game:a", 5);
        let bulk = |list: &[&str], at| {
            let list: Vec<String> = list.iter().map(|v| v.to_string()).collect();
            bulk_attestation_signing_bytes(&s, subject, &list, at)
        };
        assert_eq!(bulk(&["a", "b"], T), bulk(&["a", "b"], T));
        assert_ne!(bulk(&["a", "b"], T), bulk(&["b", "a"], T));
        assert_ne!(bulk(&["ab", "c"], T), bulk(&["a", "bc"], T));
        assert_ne!(bulk(&["a", "b"], T), bulk(&["a", "b"], T + 1));
        assert_ne!(bulk(&["a"], T), bulk(&["a", ""], T));
    }

    #[test]
    fn bulk_and_single_signatures_use_distinct_tags() {
        let subject = IdentityId::random_for_tests();
        let s = signer("achievement", "game:a", 5);
        let single = attestation_signing_bytes(&s, subject, "x", T);
        let bulk = bulk_attestation_signing_bytes(&s, subject, &["x".to_string()], T);
        assert!(single.starts_with(b"avalon.attestation.issue\x00\x01"));
        assert!(bulk.starts_with(b"avalon.attestation.bulk_issue\x00\x01"));
        assert_ne!(single, bulk);
    }

    #[test]
    fn revocation_bytes_cover_reason_code_reason_and_key() {
        let s = signer("achievement", "game:a", 5);
        let id = AttestationId(Uuid::from_u128(9));
        let base = revocation_signing_bytes(&s, id, "cheating", &reason_hash("r"));
        assert!(base.starts_with(b"avalon.attestation.revoke\x00\x01"));
        assert_ne!(
            base,
            revocation_signing_bytes(&s, id, "mistake", &reason_hash("r"))
        );
        assert_ne!(
            base,
            revocation_signing_bytes(&s, id, "cheating", &reason_hash("s"))
        );
        assert_ne!(
            base,
            revocation_signing_bytes(
                &s,
                AttestationId(Uuid::from_u128(8)),
                "cheating",
                &reason_hash("r")
            )
        );
        assert_ne!(
            base,
            revocation_signing_bytes(
                &signer("achievement", "game:a", 6),
                id,
                "cheating",
                &reason_hash("r")
            )
        );
        assert_ne!(
            revocation_signing_bytes(&s, id, "a", &reason_hash("b:c")),
            revocation_signing_bytes(&s, id, "a:b", &reason_hash("c"))
        );
    }

    #[test]
    fn issued_at_micros_truncates_to_the_stored_precision() {
        let at = OffsetDateTime::from_unix_timestamp_nanos(1_700_000_000_123_456_789).unwrap();
        assert_eq!(issued_at_micros(at), 1_700_000_000_123_456);
    }
}

/// An attestation's point-in-time status — never a mutable field on the
/// attestation itself. Computed from whether a revocation entry exists and
/// when it took effect, not read from a flag. Reinstatement (a later entry
/// reversing a revocation) and supersession are deliberately not
/// built in this pass — no protocol event kind for either exists yet
/// (`docs/projects/backend-server/architecture/protocol-events.md` catalogues `achievement.revoked`
/// but no attestation-level "reinstated"/"superseded" kind), and neither
/// is required by scenario C (`docs/projects/backend-server/architecture/revocation.md`), the one
/// this ticket's acceptance criteria actually requires. Tracked as
/// deferred follow-up, not silently assumed unnecessary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttestationStatus {
    Active,
    Revoked,
}

/// `revoked_at`, if `Some`, is when the attestation's (at most one, for
/// now — see [`AttestationStatus`]'s own doc comment) revocation entry
/// took effect. `Revoked` iff a revocation exists and `at` is at or after
/// it — never before, so a claim's status *as of its own issuance* is
/// always `Active` regardless of what happens to it later, matching
/// scenario C's "the Hub shows both, and the flip happens exactly at the
/// revocation timestamp" requirement.
pub fn attestation_status_at(
    revoked_at: Option<OffsetDateTime>,
    at: OffsetDateTime,
) -> AttestationStatus {
    match revoked_at {
        Some(revoked_at) if at >= revoked_at => AttestationStatus::Revoked,
        _ => AttestationStatus::Active,
    }
}

/// "Is this attestation still good right now" (the second of three separate
/// questions — authentic, valid, recognized — never merged into one
/// boolean). Checks the attestation's own revocation
/// status ([`attestation_status_at`]) and the issuer's current
/// `IntegratorStatus` (`Suspended`/`Revoked`/`Deprecated` are catalogued, but
/// nothing can transition an issuer into them yet — no point-in-time
/// issuer-status history exists either, so that half of this check is
/// still "as of now", not "as of `at`"; the attestation-revocation half
/// genuinely is point-in-time). Issuer-key-validity-at-issuance is a
/// separate question, already covered by
/// [`crate::integrators::IssuerKey::is_valid_at`]/`Authenticity`
/// (`avalon_chain::attestations::verify_authenticity`), not repeated here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validity {
    Valid,
    Invalid { reason: String },
}

pub fn validity(
    issuer_status: crate::integrators::IntegratorStatus,
    attestation_status: AttestationStatus,
) -> Validity {
    use crate::integrators::IntegratorStatus;
    if attestation_status == AttestationStatus::Revoked {
        return Validity::Invalid {
            reason: "attestation has been revoked".to_string(),
        };
    }
    match issuer_status {
        IntegratorStatus::Active => Validity::Valid,
        IntegratorStatus::Suspended => Validity::Invalid {
            reason: "issuer is currently suspended".to_string(),
        },
        IntegratorStatus::Revoked => Validity::Invalid {
            reason: "issuer has been revoked".to_string(),
        },
        IntegratorStatus::Deprecated => Validity::Invalid {
            reason: "issuer is deprecated".to_string(),
        },
    }
}

/// One condition under which a [`TrustRelationship`] recognizes a claim —
/// every `Some` field narrows the match; `None` means "no restriction on
/// this axis". An empty [`TrustRelationship::scopes`] list (no entries at
/// all) means unscoped: every claim from `trusted_issuer` is recognized,
/// which is different from a single all-`None` `RecognitionScope` (same
/// practical effect, but the empty-list form is the canonical
/// "unrestricted" representation — see [`recognize`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecognitionScope {
    /// `"achievement"`/`"milestone"` ([`Issuer::claim_kind`]) — `None`
    /// matches either.
    pub claim_kind: Option<String>,
    /// A specific schema a claim's definition must declare — `None`
    /// matches any schema, including a claim with no schema at all.
    pub schema: Option<GlobalId>,
    /// Inclusive floor on the claim definition's `version` — `None` means
    /// no floor.
    pub min_version: Option<u32>,
    /// Only claims issued at or after this instant — `None` means no
    /// floor. Matches the ticket's own example: "Integrator A's achievements...
    /// issued after 2027-01".
    pub issued_after: Option<OffsetDateTime>,
}

impl RecognitionScope {
    fn matches(
        &self,
        claim_kind: &str,
        schema: Option<&GlobalId>,
        version: u32,
        issued_at: OffsetDateTime,
    ) -> bool {
        if let Some(want) = &self.claim_kind {
            if want != claim_kind {
                return false;
            }
        }
        if let Some(want) = &self.schema {
            if Some(want) != schema {
                return false;
            }
        }
        if let Some(min) = self.min_version {
            if version < min {
                return false;
            }
        }
        if let Some(after) = self.issued_after {
            if issued_at < after {
                return false;
            }
        }
        true
    }
}

/// One integrator's decision to trust another issuer's attestations,
/// optionally scoped to specific claim kinds/schemas/versions/
/// time windows. `scopes.is_empty()` means unscoped — every claim from
/// `trusted_issuer` is recognized, matching this type's original
/// unscoped-only shape exactly (additive, not a breaking change).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustRelationship {
    pub truster: IntegratorId,
    pub trusted_issuer: Issuer,
    pub established_at: OffsetDateTime,
    #[serde(default)]
    pub scopes: Vec<RecognitionScope>,
}

/// "Does *this consumer's own policy* recognize this claim" (the third of
/// the three separate questions) — evaluated entirely on the consumer's side
/// (SDK or the integrator's own code), never a boolean the server computes or
/// returns. Deliberately separate from [`Validity`]/authenticity: a claim
/// can be authentic and valid and still `NotRecognized` here, and the API
/// must be able to express exactly that (this ticket's own invariant).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recognition {
    Recognized,
    NotRecognized { reason: String },
}

#[allow(clippy::too_many_arguments)]
pub fn recognize(
    policy: &TrustRelationship,
    issuer: &Issuer,
    claim_kind: &str,
    schema: Option<&GlobalId>,
    version: u32,
    issued_at: OffsetDateTime,
) -> Recognition {
    if &policy.trusted_issuer != issuer {
        return Recognition::NotRecognized {
            reason: "no trust relationship with this issuer".to_string(),
        };
    }
    if policy.scopes.is_empty()
        || policy
            .scopes
            .iter()
            .any(|s| s.matches(claim_kind, schema, version, issued_at))
    {
        Recognition::Recognized
    } else {
        Recognition::NotRecognized {
            reason: "issuer is trusted, but this claim falls outside every recognized scope"
                .to_string(),
        }
    }
}

#[cfg(test)]
mod verification_tests {
    use super::*;
    use crate::integrators::IntegratorStatus;
    use uuid::Uuid;

    #[test]
    fn validity_is_valid_only_for_an_active_issuer_and_non_revoked_attestation() {
        assert_eq!(
            validity(IntegratorStatus::Active, AttestationStatus::Active),
            Validity::Valid
        );
        assert_ne!(
            validity(IntegratorStatus::Suspended, AttestationStatus::Active),
            Validity::Valid
        );
        assert_ne!(
            validity(IntegratorStatus::Revoked, AttestationStatus::Active),
            Validity::Valid
        );
        assert_ne!(
            validity(IntegratorStatus::Deprecated, AttestationStatus::Active),
            Validity::Valid
        );
        // A revoked attestation is invalid even under an otherwise-active
        // issuer.
        assert_ne!(
            validity(IntegratorStatus::Active, AttestationStatus::Revoked),
            Validity::Valid
        );
    }

    /// Scenario C (`docs/projects/backend-server/architecture/revocation.md`): validity flips
    /// exactly at the revocation timestamp, never before it.
    #[test]
    fn scenario_c_validity_flips_exactly_at_the_revocation_timestamp() {
        let issued_at = OffsetDateTime::UNIX_EPOCH;
        let revoked_at = OffsetDateTime::UNIX_EPOCH + time::Duration::hours(10);

        assert_eq!(
            attestation_status_at(Some(revoked_at), issued_at),
            AttestationStatus::Active,
            "still active at the moment of issuance, long before revocation"
        );
        assert_eq!(
            attestation_status_at(Some(revoked_at), revoked_at - time::Duration::seconds(1)),
            AttestationStatus::Active,
            "one second before the revocation timestamp: still active"
        );
        assert_eq!(
            attestation_status_at(Some(revoked_at), revoked_at),
            AttestationStatus::Revoked,
            "at the exact revocation timestamp: already revoked"
        );
        assert_eq!(
            attestation_status_at(Some(revoked_at), revoked_at + time::Duration::hours(100)),
            AttestationStatus::Revoked,
            "long after revocation: still revoked"
        );
    }

    #[test]
    fn an_attestation_with_no_revocation_is_always_active() {
        assert_eq!(
            attestation_status_at(None, OffsetDateTime::now_utc()),
            AttestationStatus::Active
        );
    }

    fn issuer_and_scope_fixture() -> (Issuer, GlobalId) {
        let issuer = Issuer::Game(IntegratorId(Uuid::new_v4()));
        let schema = GlobalId::new("game", "ashen-realms", "schema", "v1");
        (issuer, schema)
    }

    #[test]
    fn an_unscoped_relationship_recognizes_everything_from_the_trusted_issuer() {
        let (issuer, schema) = issuer_and_scope_fixture();
        let policy = TrustRelationship {
            truster: IntegratorId(Uuid::new_v4()),
            trusted_issuer: issuer.clone(),
            established_at: OffsetDateTime::UNIX_EPOCH,
            scopes: vec![],
        };
        assert_eq!(
            recognize(
                &policy,
                &issuer,
                "achievement",
                Some(&schema),
                1,
                OffsetDateTime::UNIX_EPOCH
            ),
            Recognition::Recognized
        );
    }

    #[test]
    fn a_different_issuer_is_never_recognized_regardless_of_scopes() {
        let (issuer, _schema) = issuer_and_scope_fixture();
        let other_issuer = Issuer::Game(IntegratorId(Uuid::new_v4()));
        let policy = TrustRelationship {
            truster: IntegratorId(Uuid::new_v4()),
            trusted_issuer: issuer,
            established_at: OffsetDateTime::UNIX_EPOCH,
            scopes: vec![],
        };
        assert_eq!(
            recognize(
                &policy,
                &other_issuer,
                "achievement",
                None,
                1,
                OffsetDateTime::UNIX_EPOCH
            ),
            Recognition::NotRecognized {
                reason: "no trust relationship with this issuer".to_string()
            }
        );
    }

    #[test]
    fn scoped_by_claim_kind_rejects_the_other_kind() {
        let (issuer, _schema) = issuer_and_scope_fixture();
        let policy = TrustRelationship {
            truster: IntegratorId(Uuid::new_v4()),
            trusted_issuer: issuer.clone(),
            established_at: OffsetDateTime::UNIX_EPOCH,
            scopes: vec![RecognitionScope {
                claim_kind: Some("achievement".to_string()),
                schema: None,
                min_version: None,
                issued_after: None,
            }],
        };
        assert_eq!(
            recognize(
                &policy,
                &issuer,
                "achievement",
                None,
                1,
                OffsetDateTime::UNIX_EPOCH
            ),
            Recognition::Recognized
        );
        assert!(matches!(
            recognize(
                &policy,
                &issuer,
                "milestone",
                None,
                1,
                OffsetDateTime::UNIX_EPOCH
            ),
            Recognition::NotRecognized { .. }
        ));
    }

    #[test]
    fn scoped_by_schema_rejects_a_different_or_missing_schema() {
        let (issuer, schema) = issuer_and_scope_fixture();
        let other_schema = GlobalId::new("game", "ashen-realms", "schema", "v2");
        let policy = TrustRelationship {
            truster: IntegratorId(Uuid::new_v4()),
            trusted_issuer: issuer.clone(),
            established_at: OffsetDateTime::UNIX_EPOCH,
            scopes: vec![RecognitionScope {
                claim_kind: None,
                schema: Some(schema.clone()),
                min_version: None,
                issued_after: None,
            }],
        };
        assert_eq!(
            recognize(
                &policy,
                &issuer,
                "achievement",
                Some(&schema),
                1,
                OffsetDateTime::UNIX_EPOCH
            ),
            Recognition::Recognized
        );
        assert!(matches!(
            recognize(
                &policy,
                &issuer,
                "achievement",
                Some(&other_schema),
                1,
                OffsetDateTime::UNIX_EPOCH
            ),
            Recognition::NotRecognized { .. }
        ));
        assert!(matches!(
            recognize(
                &policy,
                &issuer,
                "achievement",
                None,
                1,
                OffsetDateTime::UNIX_EPOCH
            ),
            Recognition::NotRecognized { .. }
        ));
    }

    #[test]
    fn scoped_by_min_version_rejects_older_versions() {
        let (issuer, _schema) = issuer_and_scope_fixture();
        let policy = TrustRelationship {
            truster: IntegratorId(Uuid::new_v4()),
            trusted_issuer: issuer.clone(),
            established_at: OffsetDateTime::UNIX_EPOCH,
            scopes: vec![RecognitionScope {
                claim_kind: None,
                schema: None,
                min_version: Some(3),
                issued_after: None,
            }],
        };
        assert!(matches!(
            recognize(
                &policy,
                &issuer,
                "achievement",
                None,
                2,
                OffsetDateTime::UNIX_EPOCH
            ),
            Recognition::NotRecognized { .. }
        ));
        assert_eq!(
            recognize(
                &policy,
                &issuer,
                "achievement",
                None,
                3,
                OffsetDateTime::UNIX_EPOCH
            ),
            Recognition::Recognized
        );
    }

    #[test]
    fn scoped_by_time_window_rejects_claims_issued_too_early() {
        let (issuer, _schema) = issuer_and_scope_fixture();
        let cutoff = OffsetDateTime::UNIX_EPOCH + time::Duration::days(365);
        let policy = TrustRelationship {
            truster: IntegratorId(Uuid::new_v4()),
            trusted_issuer: issuer.clone(),
            established_at: OffsetDateTime::UNIX_EPOCH,
            scopes: vec![RecognitionScope {
                claim_kind: None,
                schema: None,
                min_version: None,
                issued_after: Some(cutoff),
            }],
        };
        assert!(matches!(
            recognize(
                &policy,
                &issuer,
                "achievement",
                None,
                1,
                OffsetDateTime::UNIX_EPOCH
            ),
            Recognition::NotRecognized { .. }
        ));
        assert_eq!(
            recognize(&policy, &issuer, "achievement", None, 1, cutoff),
            Recognition::Recognized
        );
    }

    #[test]
    fn multiple_scopes_are_or_combined() {
        let (issuer, _schema) = issuer_and_scope_fixture();
        let policy = TrustRelationship {
            truster: IntegratorId(Uuid::new_v4()),
            trusted_issuer: issuer.clone(),
            established_at: OffsetDateTime::UNIX_EPOCH,
            scopes: vec![
                RecognitionScope {
                    claim_kind: Some("achievement".to_string()),
                    schema: None,
                    min_version: None,
                    issued_after: None,
                },
                RecognitionScope {
                    claim_kind: Some("milestone".to_string()),
                    schema: None,
                    min_version: None,
                    issued_after: None,
                },
            ],
        };
        assert_eq!(
            recognize(
                &policy,
                &issuer,
                "milestone",
                None,
                1,
                OffsetDateTime::UNIX_EPOCH
            ),
            Recognition::Recognized
        );
    }
}
