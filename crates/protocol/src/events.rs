//! Protocol events — the subset of what happens in an integrator that Avalon
//! considers interoperable or durable.
//!
//! Not every integrator action is a protocol event; ordinary gameplay (combat,
//! movement, XP ticks) never becomes one. See `docs/stakeholders/Proposal.md` §14 and
//! `docs/projects/backend-server/architecture/protocol-events.md` for the hot-data/durable-fact
//! distinction, and issue #75 for why durable history is canonical.

use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::ids::GlobalId;

/// A `ProtocolEvent::kind`, enum-backed with a permanent-string mapping —
/// issue #82, reusing `Capability`'s own template (see that type's doc
/// comment for the full rationale). `Other(String)` is the escape hatch:
/// an unrecognized wire string round-trips through it rather than
/// erroring, so a new kind never needs a protocol version bump and an
/// older build never breaks on a kind it doesn't know about yet.
///
/// **The wire string (`as_str()`) is the permanent identifier, the Rust
/// variant name is not.** These strings are already durable ledger
/// content that must decode forever — renaming a variant is always safe;
/// changing what string it maps to, once anything has shipped referencing
/// it, is not.
///
/// [`Self::KNOWN`] lists every kind the codebase actually emits today
/// (`docs/projects/backend-server/architecture/protocol-events-catalogue.md` is the normative,
/// human-readable form of the same list, payload shape included) — not
/// every kind ever *proposed*. A documented-but-unbuilt future kind
/// (`issuer.key_expired`, the `issuer.suspended`/`.reinstated`/`.revoked`/
/// `.deprecated` family, `attestation.superseded`,
/// `game_event.result_issued`) gets a real variant exactly when its
/// emitter is actually built, never speculatively ahead of that — `Other`
/// already covers "not yet known to this build" without one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtocolEventKindVariant {
    IdentityCreated,
    IdentitySigningKeyAdded,
    IdentitySigningKeyRevoked,
    IdentityPasskeyRegistered,
    IdentityPasskeyRevoked,
    IdentityRecoveryConfigured,
    IdentityRecoveryRequested,
    IdentityRecoveryApproved,
    IdentityRecoveryCancelled,
    IdentityRecovered,
    ProfileUpdated,
    GameRegistered,
    GameBindingEstablished,
    GameBindingEnded,
    PermissionGranted,
    PermissionRevoked,
    IssuerRegistered,
    IssuerKeyAdded,
    IssuerKeyRevoked,
    FriendRequested,
    FriendAccepted,
    FriendRemoved,
    FriendRelationshipReversed,
    GuildCreated,
    GuildUpdated,
    GuildRoleDefined,
    GuildRoleDeleted,
    GuildMemberAdded,
    GuildMemberRemoved,
    GuildMembershipReversed,
    GuildRoleChanged,
    GuildOwnerTransferred,
    GuildGameAssociated,
    GuildFavoriteGamesUpdated,
    GuildChannelCreated,
    GuildChannelRenamed,
    GuildChannelArchived,
    GameSchemaPublished,
    GameSchemaMappingPublished,
    GameDataPublished,
    GameDataDeleted,
    AchievementDefined,
    AchievementDefinitionUpdated,
    AchievementDefinitionRetired,
    AchievementIssued,
    AchievementRevoked,
    MilestoneDefined,
    MilestoneDefinitionUpdated,
    MilestoneDefinitionRetired,
    MilestoneIssued,
    MilestoneRevoked,
    IntegratorRecognitionPublished,
    IntegratorRecognitionRevoked,
}

/// A `ProtocolEvent::kind` — either one of [`ProtocolEventKindVariant`]'s
/// known kinds, or [`ProtocolEventKind::Other`] for anything not yet known
/// to this build. See `ProtocolEventKindVariant`'s own doc comment for the
/// full rationale.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProtocolEventKind {
    Known(ProtocolEventKindVariant),
    Other(String),
}

impl ProtocolEventKind {
    /// Every known (non-`Other`) kind — the one place that enumerates the
    /// full current set, for anything that wants to iterate them (a
    /// fixture-coverage test, a future dispatcher) instead of hand-listing
    /// strings.
    pub const KNOWN: &'static [ProtocolEventKindVariant] = &[
        ProtocolEventKindVariant::IdentityCreated,
        ProtocolEventKindVariant::IdentitySigningKeyAdded,
        ProtocolEventKindVariant::IdentitySigningKeyRevoked,
        ProtocolEventKindVariant::IdentityPasskeyRegistered,
        ProtocolEventKindVariant::IdentityPasskeyRevoked,
        ProtocolEventKindVariant::IdentityRecoveryConfigured,
        ProtocolEventKindVariant::IdentityRecoveryRequested,
        ProtocolEventKindVariant::IdentityRecoveryApproved,
        ProtocolEventKindVariant::IdentityRecoveryCancelled,
        ProtocolEventKindVariant::IdentityRecovered,
        ProtocolEventKindVariant::ProfileUpdated,
        ProtocolEventKindVariant::GameRegistered,
        ProtocolEventKindVariant::GameBindingEstablished,
        ProtocolEventKindVariant::GameBindingEnded,
        ProtocolEventKindVariant::PermissionGranted,
        ProtocolEventKindVariant::PermissionRevoked,
        ProtocolEventKindVariant::IssuerRegistered,
        ProtocolEventKindVariant::IssuerKeyAdded,
        ProtocolEventKindVariant::IssuerKeyRevoked,
        ProtocolEventKindVariant::FriendRequested,
        ProtocolEventKindVariant::FriendAccepted,
        ProtocolEventKindVariant::FriendRemoved,
        ProtocolEventKindVariant::FriendRelationshipReversed,
        ProtocolEventKindVariant::GuildCreated,
        ProtocolEventKindVariant::GuildUpdated,
        ProtocolEventKindVariant::GuildRoleDefined,
        ProtocolEventKindVariant::GuildRoleDeleted,
        ProtocolEventKindVariant::GuildMemberAdded,
        ProtocolEventKindVariant::GuildMemberRemoved,
        ProtocolEventKindVariant::GuildMembershipReversed,
        ProtocolEventKindVariant::GuildRoleChanged,
        ProtocolEventKindVariant::GuildOwnerTransferred,
        ProtocolEventKindVariant::GuildGameAssociated,
        ProtocolEventKindVariant::GuildFavoriteGamesUpdated,
        ProtocolEventKindVariant::GuildChannelCreated,
        ProtocolEventKindVariant::GuildChannelRenamed,
        ProtocolEventKindVariant::GuildChannelArchived,
        ProtocolEventKindVariant::GameSchemaPublished,
        ProtocolEventKindVariant::GameSchemaMappingPublished,
        ProtocolEventKindVariant::GameDataPublished,
        ProtocolEventKindVariant::GameDataDeleted,
        ProtocolEventKindVariant::AchievementDefined,
        ProtocolEventKindVariant::AchievementDefinitionUpdated,
        ProtocolEventKindVariant::AchievementDefinitionRetired,
        ProtocolEventKindVariant::AchievementIssued,
        ProtocolEventKindVariant::AchievementRevoked,
        ProtocolEventKindVariant::MilestoneDefined,
        ProtocolEventKindVariant::MilestoneDefinitionUpdated,
        ProtocolEventKindVariant::MilestoneDefinitionRetired,
        ProtocolEventKindVariant::MilestoneIssued,
        ProtocolEventKindVariant::MilestoneRevoked,
        ProtocolEventKindVariant::IntegratorRecognitionPublished,
        ProtocolEventKindVariant::IntegratorRecognitionRevoked,
    ];

    /// The permanent wire string this kind (de)serializes as. See this
    /// type's own doc comment: this string, not the variant name, is the
    /// identifier that must never change once shipped.
    pub fn as_str(&self) -> &str {
        match self {
            ProtocolEventKind::Known(v) => v.as_str(),
            ProtocolEventKind::Other(s) => s,
        }
    }
}

impl ProtocolEventKindVariant {
    pub fn as_str(&self) -> &'static str {
        match self {
            ProtocolEventKindVariant::IdentityCreated => "identity.created",
            ProtocolEventKindVariant::IdentitySigningKeyAdded => "identity.signing_key_added",
            ProtocolEventKindVariant::IdentitySigningKeyRevoked => "identity.signing_key_revoked",
            ProtocolEventKindVariant::IdentityPasskeyRegistered => "identity.passkey_registered",
            ProtocolEventKindVariant::IdentityPasskeyRevoked => "identity.passkey_revoked",
            ProtocolEventKindVariant::IdentityRecoveryConfigured => "identity.recovery_configured",
            ProtocolEventKindVariant::IdentityRecoveryRequested => "identity.recovery_requested",
            ProtocolEventKindVariant::IdentityRecoveryApproved => "identity.recovery_approved",
            ProtocolEventKindVariant::IdentityRecoveryCancelled => "identity.recovery_cancelled",
            ProtocolEventKindVariant::IdentityRecovered => "identity.recovered",
            ProtocolEventKindVariant::ProfileUpdated => "profile.updated",
            ProtocolEventKindVariant::GameRegistered => "game.registered",
            ProtocolEventKindVariant::GameBindingEstablished => "game.binding_established",
            ProtocolEventKindVariant::GameBindingEnded => "game.binding_ended",
            ProtocolEventKindVariant::PermissionGranted => "permission.granted",
            ProtocolEventKindVariant::PermissionRevoked => "permission.revoked",
            ProtocolEventKindVariant::IssuerRegistered => "issuer.registered",
            ProtocolEventKindVariant::IssuerKeyAdded => "issuer.key_added",
            ProtocolEventKindVariant::IssuerKeyRevoked => "issuer.key_revoked",
            ProtocolEventKindVariant::FriendRequested => "friend.requested",
            ProtocolEventKindVariant::FriendAccepted => "friend.accepted",
            ProtocolEventKindVariant::FriendRemoved => "friend.removed",
            ProtocolEventKindVariant::FriendRelationshipReversed => "friend.relationship_reversed",
            ProtocolEventKindVariant::GuildCreated => "guild.created",
            ProtocolEventKindVariant::GuildUpdated => "guild.updated",
            ProtocolEventKindVariant::GuildRoleDefined => "guild.role_defined",
            ProtocolEventKindVariant::GuildRoleDeleted => "guild.role_deleted",
            ProtocolEventKindVariant::GuildMemberAdded => "guild.member_added",
            ProtocolEventKindVariant::GuildMemberRemoved => "guild.member_removed",
            ProtocolEventKindVariant::GuildMembershipReversed => "guild.membership_reversed",
            ProtocolEventKindVariant::GuildRoleChanged => "guild.role_changed",
            ProtocolEventKindVariant::GuildOwnerTransferred => "guild.owner_transferred",
            ProtocolEventKindVariant::GuildGameAssociated => "guild.game_associated",
            ProtocolEventKindVariant::GuildFavoriteGamesUpdated => "guild.favorite_games_updated",
            ProtocolEventKindVariant::GuildChannelCreated => "guild.channel_created",
            ProtocolEventKindVariant::GuildChannelRenamed => "guild.channel_renamed",
            ProtocolEventKindVariant::GuildChannelArchived => "guild.channel_archived",
            ProtocolEventKindVariant::GameSchemaPublished => "game_schema.published",
            ProtocolEventKindVariant::GameSchemaMappingPublished => "game_schema_mapping.published",
            ProtocolEventKindVariant::GameDataPublished => "game_data.published",
            ProtocolEventKindVariant::GameDataDeleted => "game_data.deleted",
            ProtocolEventKindVariant::AchievementDefined => "achievement.defined",
            ProtocolEventKindVariant::AchievementDefinitionUpdated => {
                "achievement.definition_updated"
            }
            ProtocolEventKindVariant::AchievementDefinitionRetired => {
                "achievement.definition_retired"
            }
            ProtocolEventKindVariant::AchievementIssued => "achievement.issued",
            ProtocolEventKindVariant::AchievementRevoked => "achievement.revoked",
            ProtocolEventKindVariant::MilestoneDefined => "milestone.defined",
            ProtocolEventKindVariant::MilestoneDefinitionUpdated => "milestone.definition_updated",
            ProtocolEventKindVariant::MilestoneDefinitionRetired => "milestone.definition_retired",
            ProtocolEventKindVariant::MilestoneIssued => "milestone.issued",
            ProtocolEventKindVariant::MilestoneRevoked => "milestone.revoked",
            ProtocolEventKindVariant::IntegratorRecognitionPublished => {
                "integrator.recognition_published"
            }
            ProtocolEventKindVariant::IntegratorRecognitionRevoked => {
                "integrator.recognition_revoked"
            }
        }
    }
}

impl fmt::Display for ProtocolEventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Infallible on purpose — an unrecognized string is a valid
/// `ProtocolEventKind` (`Other`), never a parse error. `Err` is
/// `Infallible` rather than `()` or a real error type so the type system
/// itself documents that this conversion cannot fail — an indexer must
/// never drop an event just because it doesn't recognize its kind (see
/// `docs/projects/backend-server/architecture/protocol-events.md`'s versioning policy).
impl FromStr for ProtocolEventKind {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        for known in ProtocolEventKind::KNOWN {
            if known.as_str() == s {
                return Ok(ProtocolEventKind::Known(*known));
            }
        }
        Ok(ProtocolEventKind::Other(s.to_string()))
    }
}

impl From<&str> for ProtocolEventKind {
    fn from(s: &str) -> Self {
        // Infallible per FromStr above.
        s.parse().unwrap_or_else(|_: Infallible| unreachable!())
    }
}

impl From<String> for ProtocolEventKind {
    fn from(s: String) -> Self {
        ProtocolEventKind::from(s.as_str())
    }
}

impl From<ProtocolEventKindVariant> for ProtocolEventKind {
    fn from(v: ProtocolEventKindVariant) -> Self {
        ProtocolEventKind::Known(v)
    }
}

impl Serialize for ProtocolEventKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ProtocolEventKind {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(ProtocolEventKind::from(s))
    }
}

/// One event's position in its identity's own per-identity chain —
/// `crate::identity_chain`'s deterministic conflict rule operates on
/// exactly these two numbers plus the event's own content hash (recomputed
/// by `identity_chain::compute_event_hash`, never stored redundantly here).
///
/// `prev_hash` is hex-encoded on the wire (`sha256` digest,
/// `identity_chain::EventHash`), matching every other hash-as-string
/// convention in this codebase (`SignedTreeHead::root_hash`,
/// `WitnessCosignature::root_hash`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentityChainPosition {
    /// Monotonic within this one identity's own chain — unrelated to the
    /// global ledger sequence a node commits events in.
    pub seq: u64,
    /// The previous event's hash in this identity's chain; `None` only for
    /// `seq == 1`, the chain's genesis event.
    pub prev_hash: Option<String>,
}

/// A durable, versioned fact Avalon considers part of protocol history.
///
/// Examples: `achievement.issued`, `guild.created`, `guild.member_added`,
/// `game.registered`, `attestation.issued`. `kind` stays a plain `String`
/// on the wire/storage type itself (unchanged shape, so the ledger schema
/// and every existing consumer keep working byte-for-byte) — domain code
/// builds/matches it through [`ProtocolEventKind`] instead of
/// hand-typing strings, but the type stored and hashed is still exactly
/// the string it always was.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolEvent {
    pub id: Uuid,
    pub kind: String,
    pub issuer: GlobalId,
    pub subject: GlobalId,
    pub payload: serde_json::Value,
    pub timestamp: OffsetDateTime,
    pub version: u32,
    /// This event's place in its identity's own chain
    /// (`crate::identity_chain`) — populated for layer-1 events whose kind
    /// classifies to `Some(_)` under `identity_chain::ActionClass::classify`
    /// (profile edits, friend actions, guild-membership changes, key/
    /// recovery events), `None` for every other kind. Additive: a `None`
    /// here behaves exactly as every event did before this field existed,
    /// so already-emitted history and any event kind that never
    /// participates in a chain are unaffected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_chain: Option<IdentityChainPosition>,
}

/// A group of protocol events committed together, so a durable commitment can
/// represent many logical events without one settlement action per event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBatch {
    pub id: Uuid,
    pub events: Vec<ProtocolEvent>,
    pub created_at: OffsetDateTime,
}

/// A durable commitment to an `EventBatch`, produced by whatever
/// `SettlementProvider` implementation is in use (see the `chain` crate).
/// Deliberately opaque here: this crate does not know or care whether
/// `proof` is a database row's signature or a Merkle root anchored on-chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commitment {
    pub batch_id: Uuid,
    pub proof: Vec<u8>,
    pub committed_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the exact wire string every known variant maps to — the test
    /// that catches an accidental rename of the *wire* string, which must
    /// never happen once anything ships (see this module's doc comment).
    #[test]
    fn known_variants_map_to_their_permanent_wire_string() {
        let expected: &[(ProtocolEventKindVariant, &str)] = &[
            (
                ProtocolEventKindVariant::IdentityCreated,
                "identity.created",
            ),
            (
                ProtocolEventKindVariant::IdentitySigningKeyAdded,
                "identity.signing_key_added",
            ),
            (
                ProtocolEventKindVariant::IdentitySigningKeyRevoked,
                "identity.signing_key_revoked",
            ),
            (
                ProtocolEventKindVariant::IdentityPasskeyRegistered,
                "identity.passkey_registered",
            ),
            (
                ProtocolEventKindVariant::IdentityPasskeyRevoked,
                "identity.passkey_revoked",
            ),
            (
                ProtocolEventKindVariant::IdentityRecoveryConfigured,
                "identity.recovery_configured",
            ),
            (
                ProtocolEventKindVariant::IdentityRecoveryRequested,
                "identity.recovery_requested",
            ),
            (
                ProtocolEventKindVariant::IdentityRecoveryApproved,
                "identity.recovery_approved",
            ),
            (
                ProtocolEventKindVariant::IdentityRecoveryCancelled,
                "identity.recovery_cancelled",
            ),
            (
                ProtocolEventKindVariant::IdentityRecovered,
                "identity.recovered",
            ),
            (ProtocolEventKindVariant::ProfileUpdated, "profile.updated"),
            (ProtocolEventKindVariant::GameRegistered, "game.registered"),
            (
                ProtocolEventKindVariant::GameBindingEstablished,
                "game.binding_established",
            ),
            (
                ProtocolEventKindVariant::GameBindingEnded,
                "game.binding_ended",
            ),
            (
                ProtocolEventKindVariant::PermissionGranted,
                "permission.granted",
            ),
            (
                ProtocolEventKindVariant::PermissionRevoked,
                "permission.revoked",
            ),
            (
                ProtocolEventKindVariant::IssuerRegistered,
                "issuer.registered",
            ),
            (ProtocolEventKindVariant::IssuerKeyAdded, "issuer.key_added"),
            (
                ProtocolEventKindVariant::IssuerKeyRevoked,
                "issuer.key_revoked",
            ),
            (
                ProtocolEventKindVariant::FriendRequested,
                "friend.requested",
            ),
            (ProtocolEventKindVariant::FriendAccepted, "friend.accepted"),
            (ProtocolEventKindVariant::FriendRemoved, "friend.removed"),
            (
                ProtocolEventKindVariant::FriendRelationshipReversed,
                "friend.relationship_reversed",
            ),
            (ProtocolEventKindVariant::GuildCreated, "guild.created"),
            (ProtocolEventKindVariant::GuildUpdated, "guild.updated"),
            (
                ProtocolEventKindVariant::GuildRoleDefined,
                "guild.role_defined",
            ),
            (
                ProtocolEventKindVariant::GuildRoleDeleted,
                "guild.role_deleted",
            ),
            (
                ProtocolEventKindVariant::GuildMemberAdded,
                "guild.member_added",
            ),
            (
                ProtocolEventKindVariant::GuildMemberRemoved,
                "guild.member_removed",
            ),
            (
                ProtocolEventKindVariant::GuildMembershipReversed,
                "guild.membership_reversed",
            ),
            (
                ProtocolEventKindVariant::GuildRoleChanged,
                "guild.role_changed",
            ),
            (
                ProtocolEventKindVariant::GuildOwnerTransferred,
                "guild.owner_transferred",
            ),
            (
                ProtocolEventKindVariant::GuildGameAssociated,
                "guild.game_associated",
            ),
            (
                ProtocolEventKindVariant::GuildFavoriteGamesUpdated,
                "guild.favorite_games_updated",
            ),
            (
                ProtocolEventKindVariant::GuildChannelCreated,
                "guild.channel_created",
            ),
            (
                ProtocolEventKindVariant::GuildChannelRenamed,
                "guild.channel_renamed",
            ),
            (
                ProtocolEventKindVariant::GuildChannelArchived,
                "guild.channel_archived",
            ),
            (
                ProtocolEventKindVariant::GameSchemaPublished,
                "game_schema.published",
            ),
            (
                ProtocolEventKindVariant::GameSchemaMappingPublished,
                "game_schema_mapping.published",
            ),
            (
                ProtocolEventKindVariant::GameDataPublished,
                "game_data.published",
            ),
            (
                ProtocolEventKindVariant::GameDataDeleted,
                "game_data.deleted",
            ),
            (
                ProtocolEventKindVariant::AchievementDefined,
                "achievement.defined",
            ),
            (
                ProtocolEventKindVariant::AchievementDefinitionUpdated,
                "achievement.definition_updated",
            ),
            (
                ProtocolEventKindVariant::AchievementDefinitionRetired,
                "achievement.definition_retired",
            ),
            (
                ProtocolEventKindVariant::AchievementIssued,
                "achievement.issued",
            ),
            (
                ProtocolEventKindVariant::AchievementRevoked,
                "achievement.revoked",
            ),
            (
                ProtocolEventKindVariant::MilestoneDefined,
                "milestone.defined",
            ),
            (
                ProtocolEventKindVariant::MilestoneDefinitionUpdated,
                "milestone.definition_updated",
            ),
            (
                ProtocolEventKindVariant::MilestoneDefinitionRetired,
                "milestone.definition_retired",
            ),
            (
                ProtocolEventKindVariant::MilestoneIssued,
                "milestone.issued",
            ),
            (
                ProtocolEventKindVariant::MilestoneRevoked,
                "milestone.revoked",
            ),
            (
                ProtocolEventKindVariant::IntegratorRecognitionPublished,
                "integrator.recognition_published",
            ),
            (
                ProtocolEventKindVariant::IntegratorRecognitionRevoked,
                "integrator.recognition_revoked",
            ),
        ];
        assert_eq!(expected.len(), ProtocolEventKind::KNOWN.len());
        for (variant, wire) in expected {
            assert_eq!(variant.as_str(), *wire);
        }
    }

    #[test]
    fn every_known_variant_round_trips_through_its_string() {
        for variant in ProtocolEventKind::KNOWN {
            let round_tripped: ProtocolEventKind = variant.as_str().parse().unwrap();
            assert_eq!(round_tripped, ProtocolEventKind::Known(*variant));
        }
    }

    #[test]
    fn an_unrecognized_string_round_trips_through_other_with_no_data_loss() {
        let kind: ProtocolEventKind = "some.future.kind".parse().unwrap();
        assert_eq!(
            kind,
            ProtocolEventKind::Other("some.future.kind".to_string())
        );
        assert_eq!(kind.as_str(), "some.future.kind");
    }

    #[test]
    fn serde_round_trips_through_the_string_not_the_variant_name() {
        let json = serde_json::to_string(&ProtocolEventKind::Known(
            ProtocolEventKindVariant::GuildCreated,
        ))
        .unwrap();
        assert_eq!(json, "\"guild.created\"");
        let back: ProtocolEventKind = serde_json::from_str(&json).unwrap();
        assert_eq!(
            back,
            ProtocolEventKind::Known(ProtocolEventKindVariant::GuildCreated)
        );

        let unknown_json = "\"a.brand.new.kind\"";
        let unknown: ProtocolEventKind = serde_json::from_str(unknown_json).unwrap();
        assert_eq!(
            unknown,
            ProtocolEventKind::Other("a.brand.new.kind".to_string())
        );
    }

    #[test]
    fn no_two_known_variants_share_a_wire_string() {
        let mut seen = std::collections::HashSet::new();
        for variant in ProtocolEventKind::KNOWN {
            assert!(
                seen.insert(variant.as_str()),
                "duplicate wire string: {}",
                variant.as_str()
            );
        }
    }
}
