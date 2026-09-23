//! Explicit, capability-based permissions.
//!
//! Least privilege by default: an integrator receives only the capabilities a user
//! has actually authorized, never everything associated with an identity.
//! See `Proposal.md` §13.

use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use time::OffsetDateTime;

use crate::ids::{IdentityId, IntegratorId};

/// A single scoped permission, e.g. `presence.read`.
///
/// Enum-backed with a permanent-string mapping, not a bare `String` and not
/// a closed enum — a plain newtype-over-`String` (this type's original
/// shape) meant nothing stopped `"achievements.read"` and
/// `"achievement.read"` from both compiling and silently failing to match at
/// a `Session::require(...)` call site; a fully closed enum would need a
/// protocol version bump for every new capability. `Other(String)` is the
/// escape hatch that keeps the old shape's extensibility: an unrecognized
/// wire string round-trips through it rather than erroring, so a client
/// running an older build never breaks on a capability it doesn't know
/// about yet.
///
/// **The wire string (`as_str()`) is the permanent identifier, the Rust
/// variant name is not.** These strings already flow into durable ledger
/// payloads (permission-grant events) that must decode forever — renaming a
/// variant is always safe, changing what string it maps to, once anything
/// has shipped referencing it, is not. Serde (de)serializes through the
/// string for exactly this reason, never through the variant name.
///
/// This is the template issue #82's event-kind catalogue (and later
/// `GlobalId`'s namespace-kind segment) should reuse — same shape, applied
/// there once actually needed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Capability {
    IdentityRead,
    ProfileRead,
    FriendsRead,
    PresenceRead,
    PresencePublish,
    GuildsRead,
    GuildsChat,
    GuildsIssue,
    AchievementsRead,
    AchievementsIssue,
    /// #324/#325: the Milestone equivalent of `AchievementsIssue`, for
    /// `Issuer::App`/`Issuer::Service` — a distinct wire string so a
    /// user's consent grant reads correctly for the issuer's own
    /// vocabulary rather than granting something literally called
    /// "achievements.issue" to a non-game integrator.
    MilestonesIssue,
    AssetsRead,
    AssetsIssue,
    WalletRead,
    WalletWrite,
    MessagesRead,
    MessagesSend,
    /// Anything not yet known to this build — never dropped, never
    /// rejected.
    Other(String),
}

impl Capability {
    /// Every known (non-`Other`) variant — the one place that enumerates
    /// the full capability set, for `Session::require(...)` callers,
    /// permission-grant validation, and the Hub's consent UI to draw from
    /// instead of hand-listing strings at each call site.
    pub const KNOWN: &'static [Capability] = &[
        Capability::IdentityRead,
        Capability::ProfileRead,
        Capability::FriendsRead,
        Capability::PresenceRead,
        Capability::PresencePublish,
        Capability::GuildsRead,
        Capability::GuildsChat,
        Capability::GuildsIssue,
        Capability::AchievementsRead,
        Capability::AchievementsIssue,
        Capability::MilestonesIssue,
        Capability::AssetsRead,
        Capability::AssetsIssue,
        Capability::WalletRead,
        Capability::WalletWrite,
        Capability::MessagesRead,
        Capability::MessagesSend,
    ];

    /// The permanent wire string this capability (de)serializes as. See
    /// this type's own doc comment: this string, not the variant name, is
    /// the identifier that must never change once shipped.
    pub fn as_str(&self) -> &str {
        match self {
            Capability::IdentityRead => "identity.read",
            Capability::ProfileRead => "profile.read",
            Capability::FriendsRead => "friends.read",
            Capability::PresenceRead => "presence.read",
            Capability::PresencePublish => "presence.publish",
            Capability::GuildsRead => "guilds.read",
            Capability::GuildsChat => "guilds.chat",
            Capability::GuildsIssue => "guilds.issue",
            Capability::AchievementsRead => "achievements.read",
            Capability::AchievementsIssue => "achievements.issue",
            Capability::MilestonesIssue => "milestones.issue",
            Capability::AssetsRead => "assets.read",
            Capability::AssetsIssue => "assets.issue",
            Capability::WalletRead => "wallet.read",
            Capability::WalletWrite => "wallet.write",
            Capability::MessagesRead => "messages.read",
            Capability::MessagesSend => "messages.send",
            Capability::Other(s) => s,
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Infallible on purpose — an unrecognized string is a valid `Capability`
/// (`Other`), never a parse error. `Err` is `Infallible` rather than `()`
/// or a real error type so the type system itself documents that this
/// conversion cannot fail.
impl FromStr for Capability {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "identity.read" => Capability::IdentityRead,
            "profile.read" => Capability::ProfileRead,
            "friends.read" => Capability::FriendsRead,
            "presence.read" => Capability::PresenceRead,
            "presence.publish" => Capability::PresencePublish,
            "guilds.read" => Capability::GuildsRead,
            "guilds.chat" => Capability::GuildsChat,
            "guilds.issue" => Capability::GuildsIssue,
            "achievements.read" => Capability::AchievementsRead,
            "achievements.issue" => Capability::AchievementsIssue,
            "milestones.issue" => Capability::MilestonesIssue,
            "assets.read" => Capability::AssetsRead,
            "assets.issue" => Capability::AssetsIssue,
            "wallet.read" => Capability::WalletRead,
            "wallet.write" => Capability::WalletWrite,
            "messages.read" => Capability::MessagesRead,
            "messages.send" => Capability::MessagesSend,
            other => Capability::Other(other.to_string()),
        })
    }
}

impl From<&str> for Capability {
    fn from(s: &str) -> Self {
        // Infallible per FromStr above.
        s.parse().unwrap_or_else(|_: Infallible| unreachable!())
    }
}

impl From<String> for Capability {
    fn from(s: String) -> Self {
        Capability::from(s.as_str())
    }
}

impl Serialize for Capability {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Capability {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(Capability::from(s))
    }
}

/// A capability a user has actually granted to a specific integrator.
///
/// Revocable and scoped: this record is the entire answer to "can Integrator X do
/// Y for User Z," not an implicit `integrator_has_access_to_user = true`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionGrant {
    pub identity_id: IdentityId,
    pub integrator_id: IntegratorId,
    pub capability: Capability,
    pub granted_at: OffsetDateTime,
    pub revoked_at: Option<OffsetDateTime>,
}

impl PermissionGrant {
    pub fn is_active(&self, now: OffsetDateTime) -> bool {
        match self.revoked_at {
            Some(revoked_at) => revoked_at > now,
            None => true,
        }
    }
}

/// Who else can see a resource — orthogonal to [`PermissionGrant`], which
/// answers "what may a specific integrator do for a specific user."
/// `Visibility` answers "who, viewer-relationship-wise, gets to read this
/// at all": a stranger, any authenticated identity, a friend,
/// a fellow guild member, or nobody but the subject themselves. An
/// integrator's own read access is governed entirely by its capability
/// grant, checked separately — `Visibility` never widens or narrows that;
/// it's the answer for every *other* kind of viewer a capability grant
/// says nothing about.
///
/// The wire string (`as_str()`/`Display`), not the Rust variant name, is
/// the permanent identifier — same posture `Capability` takes, for the
/// same reason: this is stored as player-controlled settings state (see
/// `crates/server/src/visibility.rs`), and an unrecognized stored value
/// must never become a hard failure to read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Anyone, including an unauthenticated caller.
    Public,
    /// Any identity with a valid session — a stranger, but a
    /// known-to-be-real one.
    AuthenticatedOnly,
    /// Only identities currently friends with the subject (or the subject
    /// themselves).
    Friends,
    /// Only identities who are members of the relevant guild (or the
    /// subject themselves, for a subject-scoped resource with a guild
    /// context).
    GuildMembers,
    /// Nobody but the subject.
    Private,
}

impl Visibility {
    pub fn as_str(&self) -> &'static str {
        match self {
            Visibility::Public => "public",
            Visibility::AuthenticatedOnly => "authenticated_only",
            Visibility::Friends => "friends",
            Visibility::GuildMembers => "guild_members",
            Visibility::Private => "private",
        }
    }
}

impl fmt::Display for Visibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Visibility {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "public" => Ok(Visibility::Public),
            "authenticated_only" => Ok(Visibility::AuthenticatedOnly),
            "friends" => Ok(Visibility::Friends),
            "guild_members" => Ok(Visibility::GuildMembers),
            "private" => Ok(Visibility::Private),
            other => Err(format!("unrecognized visibility: {other}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the exact wire string every known variant maps to — the test
    /// that catches an accidental rename of the *wire* string, which must
    /// never happen once anything ships (see this module's doc comment).
    #[test]
    fn known_variants_map_to_their_permanent_wire_string() {
        let expected: &[(Capability, &str)] = &[
            (Capability::IdentityRead, "identity.read"),
            (Capability::ProfileRead, "profile.read"),
            (Capability::FriendsRead, "friends.read"),
            (Capability::PresenceRead, "presence.read"),
            (Capability::PresencePublish, "presence.publish"),
            (Capability::GuildsRead, "guilds.read"),
            (Capability::GuildsChat, "guilds.chat"),
            (Capability::GuildsIssue, "guilds.issue"),
            (Capability::AchievementsRead, "achievements.read"),
            (Capability::AchievementsIssue, "achievements.issue"),
            (Capability::MilestonesIssue, "milestones.issue"),
            (Capability::AssetsRead, "assets.read"),
            (Capability::AssetsIssue, "assets.issue"),
            (Capability::WalletRead, "wallet.read"),
            (Capability::WalletWrite, "wallet.write"),
            (Capability::MessagesRead, "messages.read"),
            (Capability::MessagesSend, "messages.send"),
        ];
        assert_eq!(expected.len(), Capability::KNOWN.len());
        for (capability, wire) in expected {
            assert_eq!(capability.as_str(), *wire);
        }
    }

    #[test]
    fn every_known_variant_round_trips_through_its_string() {
        for capability in Capability::KNOWN {
            let round_tripped: Capability = capability.as_str().parse().unwrap();
            assert_eq!(&round_tripped, capability);
        }
    }

    #[test]
    fn an_unrecognized_string_round_trips_through_other_with_no_data_loss() {
        let capability: Capability = "some.future.capability".parse().unwrap();
        assert_eq!(
            capability,
            Capability::Other("some.future.capability".to_string())
        );
        assert_eq!(capability.as_str(), "some.future.capability");
    }

    #[test]
    fn serde_round_trips_through_the_string_not_the_variant_name() {
        let json = serde_json::to_string(&Capability::FriendsRead).unwrap();
        assert_eq!(json, "\"friends.read\"");
        let back: Capability = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Capability::FriendsRead);

        let unknown_json = "\"a.brand.new.capability\"";
        let unknown: Capability = serde_json::from_str(unknown_json).unwrap();
        assert_eq!(
            unknown,
            Capability::Other("a.brand.new.capability".to_string())
        );
    }

    #[test]
    fn every_visibility_variant_round_trips_through_its_wire_string() {
        for variant in [
            Visibility::Public,
            Visibility::AuthenticatedOnly,
            Visibility::Friends,
            Visibility::GuildMembers,
            Visibility::Private,
        ] {
            let round_tripped: Visibility = variant.as_str().parse().unwrap();
            assert_eq!(round_tripped, variant);
        }
    }

    #[test]
    fn visibility_from_str_rejects_an_unrecognized_string() {
        assert!("not-a-real-visibility".parse::<Visibility>().is_err());
    }

    #[test]
    fn visibility_serde_round_trips_through_the_snake_case_string() {
        let json = serde_json::to_string(&Visibility::GuildMembers).unwrap();
        assert_eq!(json, "\"guild_members\"");
        let back: Visibility = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Visibility::GuildMembers);
    }
}
