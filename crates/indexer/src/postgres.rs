//! `PostgresIndexer` — the first real [`Indexer`], closing issue #42.
//!
//! Dispatches by `event.kind` to the matching module under
//! [`crate::projections`], guarded by an `indexer_applied_events(event_id)`
//! dedup table so a redelivered or replayed (rebuild) event is a no-op
//! the second time — the two-layer idempotency design in the ticket:
//! per-event dedup here, natural-key upserts inside each projection.
//!
//! Two entry points, both doing the same dispatch:
//!
//! - [`Indexer::apply`] opens its own transaction — the shape a background
//!   consumer (a future outbox-style worker, or #43's rebuild-from-events
//!   pass) uses.
//! - [`PostgresIndexer::apply_in_tx`] takes a transaction the caller already
//!   owns, so a request handler's app-data write and its projection update
//!   commit or roll back together — see `crates/server/src/handlers.rs`
//!   (`register_finish`/`update_profile`), which call this instead of
//!   writing `profiles` themselves.

use async_trait::async_trait;
use avalon_protocol::events::ProtocolEvent;
use sqlx::{PgPool, Postgres, Transaction};

use crate::projections::{
    attestations, friendships, guild_rosters, identity_passkeys, identity_signing_keys,
    integrator_bindings, integrator_data_instances, integrator_recognitions,
    integrator_schema_mappings, integrator_schemas, profiles,
};
use crate::{IndexError, Indexer};

/// Every table a `PostgresIndexer` owns — closing issue #43: this is the
/// literal list `rebuild_from_scratch` truncates before replaying, and the
/// list the disaster-recovery test snapshots/diffs. Kept as one place so
/// adding a new projection (a new module under `projections/`) has one
/// obvious spot to also register its table here, instead of a rebuild
/// silently missing it. Deliberately does not include `indexer_applied_events`
/// alongside the read-model tables in doc comments elsewhere — it's a
/// dedup ledger, not a promised-durable projection itself, but it still has
/// to be truncated for a rebuild to actually re-apply anything.
pub const PROJECTION_TABLES: &[&str] = &[
    "indexer_applied_events",
    "profiles",
    "indexer_friendships",
    "indexer_guild_members",
    "indexer_attestations",
    "indexer_integrator_bindings",
    "indexer_integrator_data_instances",
    "indexer_integrator_recognitions",
    "indexer_integrator_schema_mappings",
    "indexer_integrator_schemas",
    "indexer_identity_passkeys",
    "indexer_identity_signing_keys",
];

#[derive(Clone)]
pub struct PostgresIndexer {
    pool: PgPool,
}

impl PostgresIndexer {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Drops and rebuilds every projection table from `events` alone —
    /// a disaster-recovery proof that Postgres really is just a
    /// projection, not a second source of truth. `events` must
    /// already be in ledger `seq` order (the caller — `avalon
    /// rebuild-index`, or a test driving this directly — is the one
    /// reading `ledger_entries`, so it owns that ordering).
    ///
    /// The truncate and every event's replay all happen inside **one**
    /// transaction: a rebuild is all-or-nothing. A partial rebuild would be
    /// worse than no rebuild at all — it would silently leave a live
    /// database missing whatever hadn't been replayed yet when a later
    /// event failed to decode/apply, rather than leaving the pre-rebuild
    /// state untouched for an operator to investigate. Returns how many
    /// events were actually applied (an event whose kind no projection
    /// recognizes still counts — [`PostgresIndexer::apply_in_tx`] treats
    /// that as a deliberate no-op, not a skip worth distinguishing here).
    pub async fn rebuild_from_scratch(
        &self,
        events: &[ProtocolEvent],
    ) -> Result<usize, IndexError> {
        let mut tx = self.pool.begin().await?;
        for table in PROJECTION_TABLES {
            // `table` always comes from the fixed `PROJECTION_TABLES`
            // constant above, never external input — `AssertSqlSafe` is
            // sqlx 0.9's opt-in for a dynamic-but-not-attacker-controlled
            // query string.
            sqlx::query(sqlx::AssertSqlSafe(format!("TRUNCATE TABLE {table}")))
                .execute(&mut *tx)
                .await?;
        }

        // `identities` is the registry every projection row references, and
        // it is not truncated above. A ledger replayed into a database that
        // never held these identities (a promoted node) has to recreate the
        // rows first: later events can precede their own `identity.created`
        // in ledger order (passkey and signing-key events are committed
        // ahead of it), so creating each row while replaying is too late.
        for event in events {
            if event.kind != "identity.created" {
                continue;
            }
            let Some(identity_id) = event
                .payload
                .get("identity_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<uuid::Uuid>().ok())
            else {
                continue;
            };
            sqlx::query(
                "INSERT INTO identities (id, created_at) VALUES ($1, $2) ON CONFLICT (id) DO NOTHING",
            )
            .bind(identity_id)
            .bind(event.timestamp)
            .execute(&mut *tx)
            .await?;
        }

        for event in events {
            self.apply_in_tx(&mut tx, event).await?;
        }

        tx.commit().await?;
        Ok(events.len())
    }

    /// The real dispatch. Marks `event.id` as applied first (inside `tx`,
    /// so the claim and the projection write commit together); if it was
    /// already claimed — a redelivery, or a rebuild replaying history
    /// that's already reflected here — returns `Ok(())` without touching
    /// any projection table a second time.
    pub async fn apply_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ProtocolEvent,
    ) -> Result<(), IndexError> {
        let claimed = sqlx::query(
            "INSERT INTO indexer_applied_events (event_id) VALUES ($1) \
             ON CONFLICT DO NOTHING RETURNING event_id",
        )
        .bind(event.id)
        .fetch_optional(&mut **tx)
        .await?;

        if claimed.is_none() {
            return Ok(());
        }

        match event.kind.as_str() {
            "identity.created" | "profile.updated" => {
                if let Some(write) = profiles::decode(event) {
                    profiles::apply(tx, &write).await?;
                }
            }
            "identity.passkey_registered" | "identity.passkey_revoked" => {
                if let Some(write) = identity_passkeys::decode(event) {
                    identity_passkeys::apply(tx, &write).await?;
                }
            }
            "identity.signing_key_added" | "identity.signing_key_revoked" => {
                if let Some(write) = identity_signing_keys::decode(event) {
                    identity_signing_keys::apply(tx, &write).await?;
                }
            }
            "friend.requested"
            | "friend.accepted"
            | "friend.removed"
            | "friend.relationship_reversed" => {
                if let Some(write) = friendships::decode(event) {
                    friendships::apply(tx, &write).await?;
                }
            }
            "guild.created"
            | "guild.member_added"
            | "guild.member_removed"
            | "guild.membership_reversed"
            | "guild.role_changed" => {
                if let Some(write) = guild_rosters::decode(event) {
                    guild_rosters::apply(tx, &write).await?;
                }
            }
            "achievement.issued" | "achievement.revoked" => {
                if let Some(write) = attestations::decode(event) {
                    attestations::apply(tx, &write).await?;
                }
            }
            "game_schema.published" => {
                if let Some(write) = integrator_schemas::decode(event) {
                    integrator_schemas::apply(tx, &write).await?;
                }
            }
            "game_schema_mapping.published" => {
                if let Some(write) = integrator_schema_mappings::decode(event) {
                    integrator_schema_mappings::apply(tx, &write).await?;
                }
            }
            "game.binding_established" | "game.binding_ended" => {
                if let Some(write) = integrator_bindings::decode(event) {
                    integrator_bindings::apply(tx, &write).await?;
                }
            }
            "game_data.published" => {
                if let Some(write) = integrator_data_instances::decode(event) {
                    integrator_data_instances::apply(tx, &write).await?;
                }
            }
            "game_data.deleted" => {
                if let Some(write) = integrator_data_instances::decode_deletion(event) {
                    integrator_data_instances::apply_deletion(tx, &write).await?;
                }
            }
            "integrator.recognition_published" | "integrator.recognition_revoked" => {
                if let Some(write) = integrator_recognitions::decode(event) {
                    integrator_recognitions::apply(tx, &write).await?;
                }
            }

            // #669: known, current event kinds whose data already lives in
            // its own direct source-of-truth table, written synchronously
            // by the handler that emits the event (in the same transaction
            // as its `outbox::enqueue` call) — never through the indexer.
            // Nothing to project here, so these are a deliberate no-op
            // rather than falling into the `other` branch below, which
            // would log them as unrecognized on every rebuild even though
            // nothing is actually missing. See
            // `docs/projects/backend-server/architecture/query-and-indexing.md`'s
            // "Events with their own source of truth" section for the full
            // table-by-kind mapping and why each one is scoped this way.
            "game.registered"
            | "issuer.key_added"
            | "issuer.key_revoked"
            | "guild.channel_created"
            | "permission.granted"
            | "permission.revoked"
            | "achievement.defined"
            | "achievement.definition_updated"
            | "achievement.definition_retired"
            | "milestone.defined"
            | "milestone.definition_updated"
            | "milestone.definition_retired"
            | "milestone.issued"
            | "milestone.revoked"
            // #678: the other half of #669's sweep, verified the same way —
            // each writes its own direct table synchronously, never through
            // the indexer. See query-and-indexing.md's table for specifics.
            | "identity.recovery_configured"
            | "identity.recovery_requested"
            | "identity.recovery_approved"
            | "identity.recovery_cancelled"
            | "identity.recovered"
            | "issuer.registered"
            | "guild.updated"
            | "guild.role_defined"
            | "guild.role_deleted"
            | "guild.owner_transferred"
            | "guild.game_associated"
            | "guild.favorite_games_updated"
            | "guild.channel_renamed"
            | "guild.channel_archived" => {}

            other => {
                // Never an error — an old indexer must survive a new event
                // kind being introduced elsewhere in the protocol. See
                // docs/projects/backend-server/architecture/query-and-indexing.md.
                eprintln!("indexer: skipping unrecognized event kind {other:?}");
            }
        }

        Ok(())
    }
}

#[async_trait]
impl Indexer for PostgresIndexer {
    async fn apply(&self, event: &ProtocolEvent) -> Result<(), IndexError> {
        let mut tx = self.pool.begin().await?;
        self.apply_in_tx(&mut tx, event).await?;
        tx.commit().await?;
        Ok(())
    }
}
