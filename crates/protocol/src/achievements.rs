//! Achievements as verifiable claims (attestations), not shared database rows.
//!
//! Avalon records that an issuer made a claim about a user. It never
//! dictates what a receiving integrator does with that claim — see
//! `docs/stakeholders/Proposal.md` §8–9, `docs/architecture/achievements-and-attestations.md`, and the trust
//! model in `docs/architecture/trust-model.md` (issue #76).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{AttestationId, GlobalId, IdentityId, IntegratorId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AchievementDefinition {
    pub id: GlobalId,
    pub issuer: Issuer,
    pub name: String,
    pub description: String,
    /// An issuer-declared schema reference (e.g. an integrator-event-result schema,
    /// #88) so a consumer can recognize a claim's shape independently of the
    /// issuer's own naming for it. Optional: not every definition needs one.
    pub schema: Option<GlobalId>,
    /// Bumped on every `achievement.definition_updated`; the definition's
    /// `id` never changes, so this is what lets a consumer notice a
    /// definition evolved (see `docs/architecture/achievements-and-attestations.md`).
    pub version: u32,
}

/// Whoever is entitled to issue attestations. `Game`/`App`/`Service` mirror
/// `IntegratorCategory` (issue #282, decision #275) — additive sibling
/// variants sharing `IntegratorId`'s id space, each minting its own `GlobalId`
/// namespace prefix (`game:`/`app:`/`service:`) for events they author.
/// `Issuer::Game` itself is unchanged, per #275.
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

    /// The claim-vocabulary word this issuer's category uses (issue #324,
    /// decided; #325 built the definition-CRUD half, #32 this attestation
    /// half): `"achievement"` for `Game`, `"milestone"` for `App`/
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

/// A detached Ed25519 signature over an attestation's canonical bytes
/// (issue #32), produced by one of the issuer's own keys
/// (`avalon_protocol::integrators::IssuerKey`) — never by the node operator.
/// `key_id` names which of the issuer's (possibly several) keys signed it,
/// so verification can resolve that exact key's point-in-time validity at
/// `issued_at` (#84's `resolve_valid_signing_key`) rather than assuming the
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
/// **No `revoked_at` here** (issue #32, per #81's decided revocation
/// mechanics): a mutable status field on durable protocol history is
/// exactly what #75's ADR forbids. Revocation is its own append-only entry
/// — tracked as #85, not built yet, and deliberately not improvised here as
/// a side effect of shipping issuance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AchievementAttestation {
    pub id: AttestationId,
    pub issuer: Issuer,
    pub subject: IdentityId,
    pub achievement: GlobalId,
    pub issued_at: OffsetDateTime,
    /// Verified against the issuer's own key
    /// (`avalon_chain::attestations::verify_authenticity`, #33) before this
    /// attestation is ever stored — see [`attestation_signing_bytes`] for
    /// exactly what's signed.
    pub proof: Signature,
}

/// The exact bytes an issuer's key signs to authorize an attestation —
/// deliberately excludes `issued_at` (mirroring `identity.created`'s own
/// signing-bytes precedent, `handlers::identity_created_signing_bytes`):
/// the ledger's own event timestamp, not the signed payload, is what fixes
/// *when* an attestation was recorded, so a signature never has to commit
/// to a time before the server assigns one. `claim_kind` is `"achievement"`
/// or `"milestone"` ([`Issuer::claim_kind`]) — folded into the signed bytes
/// so a signature produced for one claim vocabulary can never be replayed
/// as if it were the other, even though the wire mechanics are identical.
pub fn attestation_signing_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    subject: IdentityId,
    achievement: &str,
) -> Vec<u8> {
    format!("avalon:{claim_kind}.issued:v1:{issuer_ref}:{subject}:{achievement}").into_bytes()
}

/// The exact bytes an issuer's key signs to authorize a *bulk* issuance
/// (issue #495, implementing #492's decided shape: N ordinary attestations
/// sharing one request/signature envelope, not a new claim-set attestation
/// type). One signature covers the whole ordered `achievements` list for
/// one `subject` — each entry is that claim's own full [`GlobalId`] wire
/// string (the same value a single [`attestation_signing_bytes`] call
/// would sign), length-prefixed so two different orderings of the same
/// keys, or a key containing bytes that could otherwise be mistaken for a
/// delimiter, can never produce identical signed bytes (same care
/// `crates/chain/src/sth.rs::signing_message` already takes with its own
/// variable-length fields). A signature over this can never be replayed
/// as a signature over a different claim list, a different subject, or a
/// different issuer/claim-kind pair.
pub fn bulk_attestation_signing_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    subject: IdentityId,
    achievements: &[String],
) -> Vec<u8> {
    let mut message =
        format!("avalon:{claim_kind}.issued.bulk:v1:{issuer_ref}:{subject}:").into_bytes();
    message.extend_from_slice(&(achievements.len() as u32).to_be_bytes());
    for achievement in achievements {
        message.extend_from_slice(&(achievement.len() as u32).to_be_bytes());
        message.extend_from_slice(achievement.as_bytes());
    }
    message
}

/// The exact bytes an issuer's key signs to authorize a revocation (issue
/// #85, implementing #81's decided mechanics: revocation is a signed,
/// appended entry — never a mutation of the original attestation).
/// `attestation_id` folded in means a revocation signature can never be
/// replayed against a different attestation; `reason_code` folded in means
/// it can't be replayed with a different claimed reason either.
pub fn revocation_signing_bytes(
    claim_kind: &str,
    issuer_ref: &str,
    attestation_id: AttestationId,
    reason_code: &str,
) -> Vec<u8> {
    format!("avalon:{claim_kind}.revoked:v1:{issuer_ref}:{attestation_id}:{reason_code}")
        .into_bytes()
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

    #[test]
    fn attestation_signing_bytes_differ_by_claim_kind_even_for_identical_fields() {
        // #32's own invariant: a signature produced under one claim
        // vocabulary must never verify under the other, even for the same
        // issuer/subject/achievement triple — the claim_kind is folded
        // into what's actually signed, not just into routing.
        let subject = IdentityId(Uuid::new_v4());
        let achievement = GlobalId::new("game", "ashen-realms", "achievement", "dragon_slayer");

        let achievement_bytes = attestation_signing_bytes(
            "achievement",
            "game:ashen-realms",
            subject,
            achievement.as_str(),
        );
        let milestone_bytes = attestation_signing_bytes(
            "milestone",
            "game:ashen-realms",
            subject,
            achievement.as_str(),
        );

        assert_ne!(achievement_bytes, milestone_bytes);
    }

    #[test]
    fn attestation_signing_bytes_are_deterministic() {
        let subject = IdentityId(Uuid::new_v4());
        let achievement = GlobalId::new("app", "wallet-app", "milestone", "onboarded");

        let a =
            attestation_signing_bytes("milestone", "app:wallet-app", subject, achievement.as_str());
        let b =
            attestation_signing_bytes("milestone", "app:wallet-app", subject, achievement.as_str());
        assert_eq!(a, b);
    }

    #[test]
    fn bulk_attestation_signing_bytes_are_deterministic() {
        let subject = IdentityId(Uuid::new_v4());
        let achievements = vec![
            "game:ashen-realms:achievement:dragon_slayer".to_string(),
            "game:ashen-realms:achievement:lost_city".to_string(),
        ];

        let a = bulk_attestation_signing_bytes(
            "achievement",
            "game:ashen-realms",
            subject,
            &achievements,
        );
        let b = bulk_attestation_signing_bytes(
            "achievement",
            "game:ashen-realms",
            subject,
            &achievements,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn bulk_attestation_signing_bytes_differ_by_claim_order() {
        // A bulk signature must not verify against a claim list the issuer
        // never actually signed, including a reordering of the same keys —
        // length-prefixing alone doesn't guarantee this unless order is
        // also part of the message, which it is (iteration order below).
        let subject = IdentityId(Uuid::new_v4());
        let forward = vec!["a".to_string(), "b".to_string()];
        let reversed = vec!["b".to_string(), "a".to_string()];

        let forward_bytes =
            bulk_attestation_signing_bytes("achievement", "game:ashen-realms", subject, &forward);
        let reversed_bytes =
            bulk_attestation_signing_bytes("achievement", "game:ashen-realms", subject, &reversed);
        assert_ne!(forward_bytes, reversed_bytes);
    }

    #[test]
    fn bulk_attestation_signing_bytes_are_unambiguous_across_a_split_boundary() {
        // Without length-prefixing, `["ab", "c"]` and `["a", "bc"]` could
        // concatenate to the same bytes. With it, they must not.
        let subject = IdentityId(Uuid::new_v4());
        let split_a = vec!["ab".to_string(), "c".to_string()];
        let split_b = vec!["a".to_string(), "bc".to_string()];

        let bytes_a =
            bulk_attestation_signing_bytes("achievement", "game:ashen-realms", subject, &split_a);
        let bytes_b =
            bulk_attestation_signing_bytes("achievement", "game:ashen-realms", subject, &split_b);
        assert_ne!(bytes_a, bytes_b);
    }

    #[test]
    fn bulk_attestation_signing_bytes_differ_from_a_single_claim_signature() {
        // A bulk signature over a one-claim list must never verify as a
        // plain single-claim `attestation_signing_bytes` signature, or vice
        // versa — the `.issued.bulk` domain tag keeps the two schemes from
        // ever being confused, even for the degenerate one-claim case.
        let subject = IdentityId(Uuid::new_v4());
        let achievement = "game:ashen-realms:achievement:dragon_slayer".to_string();

        let single =
            attestation_signing_bytes("achievement", "game:ashen-realms", subject, &achievement);
        let bulk = bulk_attestation_signing_bytes(
            "achievement",
            "game:ashen-realms",
            subject,
            std::slice::from_ref(&achievement),
        );
        assert_ne!(single, bulk);
    }
}

/// An attestation's point-in-time status (issue #85, implementing #81's
/// decided mechanics) — never a mutable field on the attestation itself.
/// Computed from whether a revocation entry exists and when it took
/// effect, not read from a flag. Reinstatement (a later entry reversing a
/// revocation, per #81's decision) and supersession are deliberately not
/// built in this pass — no protocol event kind for either exists yet
/// (`docs/architecture/protocol-events.md` catalogues `achievement.revoked`
/// but no attestation-level "reinstated"/"superseded" kind), and neither
/// is required by scenario C (`docs/architecture/revocation.md`), the one
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

/// "Is this attestation still good right now" (issue #33/#85, ADR #76's
/// second of three separate questions — authentic, valid, recognized —
/// never merged into one boolean). Checks the attestation's own revocation
/// status ([`attestation_status_at`], #85) and the issuer's current
/// `IntegratorStatus` (#84 catalogued `Suspended`/`Revoked`/`Deprecated`, but
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
/// optionally scoped (issue #33) to specific claim kinds/schemas/versions/
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

/// "Does *this consumer's own policy* recognize this claim" (issue #33,
/// ADR #76's third question) — evaluated entirely on the consumer's side
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

    /// Scenario C (`docs/architecture/revocation.md`): validity flips
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
