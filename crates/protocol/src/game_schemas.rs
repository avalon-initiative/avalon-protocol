//! Game Space schema publication (issue #255, decided by #181).
//!
//! A game may publish a description of how its own data is structured, so
//! other developers and the registry can read its shape — see
//! `docs/architecture/game-space.md`. #181 decided the representation:
//! protobuf IDL (`.proto`), stored as opaque source text. Avalon never
//! parses or compiles it — that would require real `.proto`
//! parsing/codegen for a description this crate only needs to store,
//! version, and serve back unchanged, not execute (explicitly out of scope
//! for this ticket).
//!
//! **One type, not two.** The ticket's "Affected crates" section names
//! `GameSchema`/`GameSchemaVersion` as the shape to land, but a game
//! publishes directly into a monotonic version sequence — there is no
//! schema-level row distinct from its versions the way
//! `AchievementDefinition` (a single mutable row, `schema`/`version` just a
//! reference + counter on it) has one. [`GameSchemaVersion`] *is* the
//! publication: each one is a complete, immutable, addressable fact on its
//! own, namespaced the same way `AchievementDefinition` is
//! (`game:<slug>:schema:<version>`, minted by
//! `crates/server/src/game_schemas.rs::schema_ref`). A separate `GameSchema`
//! wrapper type would have no fields of its own beyond "the game that owns
//! this stream" — already carried by `game_id` on every version — so it is
//! not built here.
//!
//! **Immutability, not deletion.** Once published, a version's `proto_source`
//! is never edited in place (this module's own invariant, enforced by
//! `crates/server/src/game_schemas.rs` never issuing an `UPDATE` against
//! that column). Evolving a schema means publishing a new
//! [`GameSchemaVersion`] with `version` one higher; the previous version's
//! `superseded_by` is set to point at it, so lineage is traceable without
//! ever rewriting the superseded row's actual schema text. This is the same
//! discipline `docs/architecture/protocol-events.md`'s versioning policy
//! already applies to `ProtocolEvent` kinds: every version stays decodable
//! forever, decoders are added, not replaced.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{GameId, GlobalId};

/// One immutable, published version of a game's declared schema
/// description. See this module's doc comment for why there is no separate
/// `GameSchema` type: `game_id` plus a monotonic `version` is the whole
/// identity a schema stream needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameSchemaVersion {
    /// `game:<slug>:schema:<version>` — immutable, globally unique.
    pub id: GlobalId,
    pub game_id: GameId,
    /// Raw `.proto` source text, stored opaque. Avalon never parses or
    /// compiles this — see module doc comment.
    pub proto_source: String,
    /// Monotonic per game, starting at 1 — same precedent
    /// `AchievementDefinition.version` already established in this crate.
    pub version: u32,
    pub published_at: OffsetDateTime,
    /// Set once a later version is published superseding this one; `None`
    /// for the current version. Never cleared once set — a version that
    /// has been superseded stays superseded, even if it was itself never
    /// the *first* version.
    pub superseded_by: Option<GlobalId>,
}

impl GameSchemaVersion {
    pub fn is_current(&self) -> bool {
        self.superseded_by.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::GlobalId as Gid;
    use uuid::Uuid;

    fn version(version: u32, superseded_by: Option<Gid>) -> GameSchemaVersion {
        GameSchemaVersion {
            id: Gid::new("game", "ashen-realms", "schema", &version.to_string()),
            game_id: GameId(Uuid::new_v4()),
            proto_source: "message Character { uint32 level = 1; }".to_string(),
            version,
            published_at: OffsetDateTime::now_utc(),
            superseded_by,
        }
    }

    #[test]
    fn a_version_with_no_superseded_by_is_current() {
        assert!(version(1, None).is_current());
    }

    #[test]
    fn a_version_with_superseded_by_set_is_not_current() {
        let successor = Gid::new("game", "ashen-realms", "schema", "2");
        assert!(!version(1, Some(successor)).is_current());
    }
}
