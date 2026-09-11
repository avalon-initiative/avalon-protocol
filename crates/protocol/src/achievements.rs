//! Achievements as verifiable claims (attestations), not shared database rows.
//!
//! Avalon records that an issuer made a claim about a player. It never
//! dictates what a receiving game does with that claim — see
//! `docs/stakeholders/Proposal.md` §8–9, `docs/architecture/achievements-and-attestations.md`, and the trust
//! model in `docs/architecture/trust-model.md` (issue #76).

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{AttestationId, GameId, GlobalId, IdentityId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AchievementDefinition {
    pub id: GlobalId,
    pub issuer: Issuer,
    pub name: String,
    pub description: String,
    /// An issuer-declared schema reference (e.g. a game-event-result schema,
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
/// variants sharing `GameId`'s id space, each minting its own `GlobalId`
/// namespace prefix (`game:`/`app:`/`service:`) for events they author.
/// `Issuer::Game` itself is unchanged, per #275.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Issuer {
    Game(GameId),
    App(GameId),
    Service(GameId),
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
    /// `Service` — matches `avalon_protocol::games::IntegratorCategory::claim_kind()`
    /// exactly (kept as its own method here rather than converting through
    /// `IntegratorCategory`, since `Issuer` is what this module's own types
    /// are already built around).
    pub fn claim_kind(&self) -> &'static str {
        match self {
            Issuer::Game(_) => "achievement",
            Issuer::App(_) | Issuer::Service(_) => "milestone",
        }
    }

    pub fn id(&self) -> GameId {
        match self {
            Issuer::Game(id) | Issuer::App(id) | Issuer::Service(id) => *id,
        }
    }
}

/// A detached Ed25519 signature over an attestation's canonical bytes
/// (issue #32), produced by one of the issuer's own keys
/// (`avalon_protocol::games::IssuerKey`) — never by the node operator.
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
/// The receiving game decides independently whether it trusts `issuer` and
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

#[cfg(test)]
mod issuer_tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn game_issuer_namespace_is_unchanged() {
        let issuer = Issuer::Game(GameId(Uuid::new_v4()));
        assert_eq!(issuer.namespace(), "game");
    }

    #[test]
    fn app_issuer_mints_the_app_namespace() {
        let id = GameId(Uuid::new_v4());
        let issuer = Issuer::App(id);
        assert_eq!(issuer.namespace(), "app");
        assert_eq!(issuer.id(), id);
    }

    #[test]
    fn service_issuer_mints_the_service_namespace() {
        let id = GameId(Uuid::new_v4());
        let issuer = Issuer::Service(id);
        assert_eq!(issuer.namespace(), "service");
        assert_eq!(issuer.id(), id);
    }

    #[test]
    fn issuer_variants_produce_distinct_global_ids_for_the_same_id() {
        let id = GameId(Uuid::new_v4());
        let owner = id.0.to_string();

        let game_ref = GlobalId::new(Issuer::Game(id).namespace(), &owner, "self", "x");
        let app_ref = GlobalId::new(Issuer::App(id).namespace(), &owner, "self", "x");
        let service_ref = GlobalId::new(Issuer::Service(id).namespace(), &owner, "self", "x");

        assert_eq!(game_ref.as_str(), format!("game:{owner}:self:x"));
        assert_eq!(app_ref.as_str(), format!("app:{owner}:self:x"));
        assert_eq!(service_ref.as_str(), format!("service:{owner}:self:x"));
        assert_ne!(game_ref, app_ref);
        assert_ne!(app_ref, service_ref);
    }

    #[test]
    fn claim_kind_matches_the_decided_vocabulary_split() {
        let id = GameId(Uuid::new_v4());
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
}

/// "Is this attestation still good right now" (issue #33, ADR #76's second
/// of three separate questions — authentic, valid, recognized — never
/// merged into one boolean). Deliberately partial today, honestly, not
/// silently: revocation/supersession is #85, not built yet, so this only
/// checks the one thing that's actually real — the issuer's own current
/// `GameStatus` (#84 catalogued `Suspended`/`Revoked`/`Deprecated`, but
/// nothing can transition into them yet, so this always resolves `Valid`
/// in practice until that lands too). No point-in-time issuer-status
/// history exists either, so this checks status *now*, not "as of `at`" —
/// a real gap against the ticket's original design, tracked rather than
/// silently approximated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Validity {
    Valid,
    Invalid { reason: String },
}

pub fn validity(issuer_status: crate::games::GameStatus) -> Validity {
    use crate::games::GameStatus;
    match issuer_status {
        GameStatus::Active => Validity::Valid,
        GameStatus::Suspended => Validity::Invalid {
            reason: "issuer is currently suspended".to_string(),
        },
        GameStatus::Revoked => Validity::Invalid {
            reason: "issuer has been revoked".to_string(),
        },
        GameStatus::Deprecated => Validity::Invalid {
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
    /// floor. Matches the ticket's own example: "Game A's achievements...
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
    pub truster: GameId,
    pub trusted_issuer: Issuer,
    pub established_at: OffsetDateTime,
    #[serde(default)]
    pub scopes: Vec<RecognitionScope>,
}

/// "Does *this consumer's own policy* recognize this claim" (issue #33,
/// ADR #76's third question) — evaluated entirely on the consumer's side
/// (SDK or the game's own code), never a boolean the server computes or
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
    use crate::games::GameStatus;
    use uuid::Uuid;

    #[test]
    fn validity_is_valid_only_for_an_active_issuer() {
        assert_eq!(validity(GameStatus::Active), Validity::Valid);
        assert_ne!(validity(GameStatus::Suspended), Validity::Valid);
        assert_ne!(validity(GameStatus::Revoked), Validity::Valid);
        assert_ne!(validity(GameStatus::Deprecated), Validity::Valid);
    }

    fn issuer_and_scope_fixture() -> (Issuer, GlobalId) {
        let issuer = Issuer::Game(GameId(Uuid::new_v4()));
        let schema = GlobalId::new("game", "ashen-realms", "schema", "v1");
        (issuer, schema)
    }

    #[test]
    fn an_unscoped_relationship_recognizes_everything_from_the_trusted_issuer() {
        let (issuer, schema) = issuer_and_scope_fixture();
        let policy = TrustRelationship {
            truster: GameId(Uuid::new_v4()),
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
        let other_issuer = Issuer::Game(GameId(Uuid::new_v4()));
        let policy = TrustRelationship {
            truster: GameId(Uuid::new_v4()),
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
            truster: GameId(Uuid::new_v4()),
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
            truster: GameId(Uuid::new_v4()),
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
            truster: GameId(Uuid::new_v4()),
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
            truster: GameId(Uuid::new_v4()),
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
            truster: GameId(Uuid::new_v4()),
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
