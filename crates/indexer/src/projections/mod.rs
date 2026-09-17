//! One module per read model, closing issue #42.
//!
//! Each module exposes the same two-part shape:
//!
//! - `decode(&ProtocolEvent) -> Option<Write>` — pure, no I/O. Turns a
//!   recognized event's JSON payload into a typed write, or returns `None`
//!   for a kind this projection doesn't handle or a payload it can't parse.
//!   `None` is never an error — [`crate::postgres::PostgresIndexer`] treats
//!   it exactly like an unrecognized event kind (see
//!   `docs/architecture/query-and-indexing.md`: "unknown kinds are logged
//!   and skipped, never an error, so old indexers survive new events").
//!   Being pure, `decode` is unit-tested directly, with no Postgres needed.
//! - `apply(&mut Transaction, &Write) -> Result<(), IndexError>` — the SQL
//!   side, an upsert keyed by the write's natural key so replaying the same
//!   write twice converges to the same row, never duplicates or drifts.

pub mod attestations;
pub mod friendships;
pub mod guild_rosters;
pub mod identity_passkeys;
pub mod integrator_bindings;
pub mod integrator_data_instances;
pub mod integrator_recognitions;
pub mod integrator_schema_mappings;
pub mod integrator_schemas;
pub mod profiles;

/// Pulls a `Uuid`-shaped string field out of an event payload. Shared by
/// every projection below since `ProtocolEvent::payload` is a bare
/// `serde_json::Value` (no typed payloads yet — issue #82), and a `Uuid`
/// always round-trips through JSON as a string.
pub(crate) fn uuid_field(payload: &serde_json::Value, key: &str) -> Option<uuid::Uuid> {
    payload.get(key)?.as_str()?.parse().ok()
}
