//! Explicit, capability-based permissions.
//!
//! Least privilege by default: an integrator receives only the capabilities
//! a user has actually authorized, never everything associated with an
//! identity.

use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A single scoped permission, e.g. `presence.read`.
///
/// Enum-backed with a permanent-string mapping, not a bare `String` and not
/// a closed enum — a plain newtype over `String` means nothing stops
/// `"achievements.read"` and `"achievement.read"` from both compiling and
/// silently failing to match at a `Session::require(...)` call site, while
/// a fully closed enum would break a client the moment the server learns a
/// new capability. [`Capability::Other`] is the escape hatch: an
/// unrecognized wire string round-trips through it rather than erroring.
///
/// **The wire string (`as_str()`) is the permanent identifier, the Rust
/// variant name is not.** Serde (de)serializes through the string for
/// exactly that reason, never through the variant name. This list has to
/// stay in step with the server's own vocabulary
/// (`avalon_protocol::permissions::Capability`) — an addition there that
/// isn't mirrored here degrades to `Other`, which still round-trips
/// losslessly, so this is a naming-convenience gap rather than a
/// correctness one.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Capability {
    /// Read the connected identity's own id.
    IdentityRead,
    /// Read the connected identity's profile.
    ProfileRead,
    /// Read the connected identity's friend list.
    FriendsRead,
    /// Read presence for identities the user's graph exposes.
    PresenceRead,
    /// Publish presence on the connected identity's behalf.
    PresencePublish,
    /// Read the connected identity's guilds, rosters, and channels.
    GuildsRead,
    /// Read and post guild chat.
    GuildsChat,
    /// Issue guild-scoped claims.
    GuildsIssue,
    /// Read the connected identity's attestation history.
    AchievementsRead,
    /// Issue achievement attestations as this integrator.
    AchievementsIssue,
    /// The Milestone equivalent of `AchievementsIssue`, for app/service
    /// issuers — a distinct wire string so a user's consent grant reads
    /// correctly for the issuer's own vocabulary.
    MilestonesIssue,
    /// Read the connected identity's assets.
    AssetsRead,
    /// Issue assets as this integrator.
    AssetsIssue,
    /// Read the connected identity's wallet.
    WalletRead,
    /// Write to the connected identity's wallet.
    WalletWrite,
    /// Read direct conversations and their messages.
    MessagesRead,
    /// Send messages on the connected identity's behalf.
    MessagesSend,
    /// Anything not yet known to this build — never dropped, never
    /// rejected.
    Other(String),
}

impl Capability {
    /// Every known (non-`Other`) variant — the one place that enumerates
    /// the full capability set, so call sites draw from it instead of
    /// hand-listing strings.
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

    /// The permanent wire string this capability (de)serializes as.
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
/// (`Other`), never a parse error.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the exact wire string every known variant maps to — the test
    /// that catches an accidental rename of the *wire* string, which must
    /// never happen once anything ships.
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

        let unknown: Capability = serde_json::from_str("\"a.brand.new.capability\"").unwrap();
        assert_eq!(
            unknown,
            Capability::Other("a.brand.new.capability".to_string())
        );
    }
}
