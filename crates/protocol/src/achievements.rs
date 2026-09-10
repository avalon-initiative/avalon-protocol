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

    pub fn id(&self) -> GameId {
        match self {
            Issuer::Game(id) | Issuer::App(id) | Issuer::Service(id) => *id,
        }
    }
}

/// A signed, verifiable claim: `issuer` claims `subject` earned `achievement`.
///
/// The receiving game decides independently whether it trusts `issuer` and
/// what the claim means to it — see `TrustRelationship` and `Proposal.md` §9.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AchievementAttestation {
    pub id: AttestationId,
    pub issuer: Issuer,
    pub subject: IdentityId,
    pub achievement: GlobalId,
    pub issued_at: OffsetDateTime,
    /// Opaque signature bytes over the rest of the attestation, produced by
    /// the issuer's registered key. Verification lives in `avalon-chain`, not
    /// here — this crate only defines the shape.
    pub proof: Vec<u8>,
    pub revoked_at: Option<OffsetDateTime>,
}

impl AchievementAttestation {
    pub fn is_valid(&self, now: OffsetDateTime) -> bool {
        match self.revoked_at {
            Some(revoked_at) => revoked_at > now,
            None => true,
        }
    }
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
}

/// One integrator's decision to trust another issuer's attestations,
/// optionally scoped to specific achievements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustRelationship {
    pub truster: GameId,
    pub trusted_issuer: Issuer,
    pub established_at: OffsetDateTime,
}
