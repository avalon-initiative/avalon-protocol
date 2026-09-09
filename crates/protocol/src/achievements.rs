//! Achievements as verifiable claims (attestations), not shared database rows.
//!
//! Avalon records that an issuer made a claim about a player. It never
//! dictates what a receiving game does with that claim — see `docs/Proposal.md`
//! §8–9, `docs/architecture/achievements-and-attestations.md`, and the trust
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

/// Whoever is entitled to issue attestations — currently always a registered
/// game, but modeled as its own type since other issuer kinds (e.g. a network
/// operator issuing a network-level attestation) are plausible later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Issuer {
    Game(GameId),
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

/// One game's decision to trust another issuer's attestations, optionally
/// scoped to specific achievements.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustRelationship {
    pub truster: GameId,
    pub trusted_issuer: Issuer,
    pub established_at: OffsetDateTime,
}
