//! Schema-to-schema mapping model. See `crates/protocol/src/integrator_schemas.rs`'s
//! own module doc comment for the schema-version model this builds on.
//!
//! **Not an execution engine.** A [`IntegratorSchemaMapping`] documents a
//! correspondence between two of an integrator's own already-published
//! schema versions — renames, merges, splits, dropped fields, default
//! values — for a human or another developer to read. The integrator owns
//! the semantic transformation; Avalon never runs it or decides what it
//! means. `field_correspondence` covers simple field renames as a flat
//! old-field -> new-field map (mirroring `IntegratorSchemaVersion`'s own
//! `field_visibility` shape); `description` is free text for whatever a
//! flat map can't capture. Neither is ever interpreted or executed by this
//! crate or the server — both are stored and served back verbatim, exactly
//! like `proto_source` already is.
//!
//! **No lineage/superseding concept, unlike schema versions.** A mapping
//! doesn't supersede a prior mapping; it documents one `(from, to)`
//! correspondence. An integrator that wants to correct a mapping publishes
//! a new one — the old one stays discoverable, immutable, same as every
//! other published fact in this crate.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::ids::{GlobalId, IntegratorId};

/// One immutable, published mapping between two of an integrator's own
/// schema versions. `from`/`to` are those versions' own `GlobalId`s
/// (`game:<slug>:schema:<version>`) — both must belong to the same
/// integrator that publishes the mapping (enforced server-side at write
/// time, the same precedent `IntegratorSchemaVersion::superseded_by`
/// already set: no DB-level foreign key, an application-level
/// existence/ownership check instead).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegratorSchemaMapping {
    /// `game:<slug>:schema_mapping:<seq>` — immutable, globally unique.
    pub id: GlobalId,
    pub integrator_id: IntegratorId,
    /// The schema version this mapping maps *from*.
    pub from: GlobalId,
    /// The schema version this mapping maps *to*.
    pub to: GlobalId,
    /// Free text documenting whatever `field_correspondence`'s flat map
    /// can't capture — merges, splits, dropped fields, default values.
    pub description: String,
    /// A simple old-field -> new-field correspondence map, for the subset
    /// of a migration that's a plain rename. Never interpreted or executed
    /// — see module doc comment.
    pub field_correspondence: BTreeMap<String, String>,
    pub published_at: OffsetDateTime,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn mapping() -> IntegratorSchemaMapping {
        IntegratorSchemaMapping {
            id: GlobalId::new("game", "ashen-realms", "schema_mapping", "1"),
            integrator_id: IntegratorId(Uuid::new_v4()),
            from: GlobalId::new("game", "ashen-realms", "schema", "1"),
            to: GlobalId::new("game", "ashen-realms", "schema", "2"),
            description: "progression fields were nested under `progression`".to_string(),
            field_correspondence: BTreeMap::from([(
                "level".to_string(),
                "progression.rank".to_string(),
            )]),
            published_at: OffsetDateTime::now_utc(),
        }
    }

    #[test]
    fn field_correspondence_round_trips_through_serde() {
        let original = mapping();
        let json = serde_json::to_string(&original).expect("should serialize");
        let reloaded: IntegratorSchemaMapping =
            serde_json::from_str(&json).expect("should deserialize");
        assert_eq!(reloaded.field_correspondence, original.field_correspondence);
        assert_eq!(reloaded.from.as_str(), "game:ashen-realms:schema:1");
        assert_eq!(reloaded.to.as_str(), "game:ashen-realms:schema:2");
    }
}
