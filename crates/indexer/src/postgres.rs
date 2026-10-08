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

use crate::identity_chain_store::{self, Recorded};
use crate::identity_proof::{self, EventOrigin, Verified};
use crate::projections::{
    attestations, friendships, guild_rosters, identity_passkeys, identity_signing_keys,
    integrator_bindings, integrator_data_instances, integrator_recognitions,
    integrator_schema_mappings, integrator_schemas, profiles,
};
use crate::{IndexError, Indexer};
use time::OffsetDateTime;

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
    "indexer_identity_signing_key_revocations",
    "indexer_identity_passkey_revocations",
    "identity_chain_events",
    "identity_chain_state",
];

/// How a rebuild went: events applied (including deliberate no-ops) and events refused or whose
/// dependencies never arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RebuildOutcome {
    pub applied: usize,
    pub refused: usize,
}

#[derive(Clone)]
pub struct PostgresIndexer {
    pool: PgPool,
    local_origin: Option<EventOrigin>,
}

impl PostgresIndexer {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            local_origin: None,
        }
    }

    /// Sets the network and shard of events this node authors itself, which
    /// [`Self::apply_in_tx`] and [`Indexer::apply`] verify against. Without it
    /// those entry points refuse every event that needs an origin.
    pub fn with_local_origin(
        mut self,
        network_id: impl Into<String>,
        shard_id: impl Into<String>,
    ) -> Self {
        self.local_origin = Some(EventOrigin::local(network_id, shard_id));
        self
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
    /// events were applied or refused (an event whose kind no projection
    /// recognizes still counts — [`PostgresIndexer::apply_in_tx`] treats
    /// that as a deliberate no-op, not a skip worth distinguishing here).
    pub async fn rebuild_from_scratch(
        &self,
        events: &[ProtocolEvent],
    ) -> Result<RebuildOutcome, IndexError> {
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

        // An event waiting on state later in the history is retried until a pass makes no progress.
        let mut pending: Vec<&ProtocolEvent> = events.iter().collect();
        let mut refused = 0usize;
        loop {
            let mut deferred = Vec::new();
            for event in &pending {
                // Each event applies in its own savepoint so a refused one rolls back completely.
                let mut savepoint = sqlx::Acquire::begin(&mut *tx).await?;
                match self
                    .apply_verified(&mut savepoint, event, self.local_origin.as_ref())
                    .await
                {
                    Ok(()) => savepoint.commit().await?,
                    Err(IndexError::Deferred(_) | IndexError::AwaitingKey(_)) => {
                        savepoint.rollback().await?;
                        deferred.push(*event);
                    }
                    // Refused by a validity rule, not a storage fault: the same event is refused
                    // on every replay, so it must not abort the whole rebuild.
                    Err(
                        IndexError::DisplayNameNotPermitted
                        | IndexError::DisplayNameTaken
                        | IndexError::Rejected(_),
                    ) => {
                        eprintln!(
                            "indexer: skipping refused event {} ({})",
                            event.id, event.kind
                        );
                        savepoint.rollback().await?;
                        refused += 1;
                    }
                    Err(other) => return Err(other),
                }
            }
            let progressed = deferred.len() < pending.len();
            pending = deferred;
            if pending.is_empty() || !progressed {
                break;
            }
        }
        for event in &pending {
            eprintln!(
                "indexer: skipping event {} ({}) whose dependencies never arrived",
                event.id, event.kind
            );
        }
        refused += pending.len();

        tx.commit().await?;
        Ok(RebuildOutcome {
            applied: events.len() - refused,
            refused,
        })
    }

    /// Applies an event this node authored itself, verified against the origin set by
    /// [`Self::with_local_origin`]. See [`Self::apply_in_tx_from`].
    pub async fn apply_in_tx(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ProtocolEvent,
    ) -> Result<(), IndexError> {
        self.apply_verified(tx, event, self.local_origin.as_ref())
            .await
    }

    /// Applies an event read from `origin`. The event is verified first
    /// ([`identity_proof::verify`]) and applied in a savepoint, so a refused or failed event
    /// leaves nothing behind: no dedup claim, identity row, profile, key or chain record.
    pub async fn apply_in_tx_from(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ProtocolEvent,
        origin: &EventOrigin,
    ) -> Result<(), IndexError> {
        self.apply_verified(tx, event, Some(origin)).await
    }

    async fn apply_verified(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ProtocolEvent,
        origin: Option<&EventOrigin>,
    ) -> Result<(), IndexError> {
        let mut savepoint = sqlx::Acquire::begin(&mut **tx).await?;
        match self.dispatch(&mut savepoint, event, origin).await {
            Ok(()) => {
                savepoint.commit().await?;
                Ok(())
            }
            Err(err) => {
                savepoint.rollback().await?;
                Err(err)
            }
        }
    }

    /// The real dispatch. Marks `event.id` as applied first (inside `tx`,
    /// so the claim and the projection write commit together); if it was
    /// already claimed — a redelivery, or a rebuild replaying history
    /// that's already reflected here — returns `Ok(())` without touching
    /// any projection table a second time.
    async fn dispatch(
        &self,
        tx: &mut Transaction<'_, Postgres>,
        event: &ProtocolEvent,
        origin: Option<&EventOrigin>,
    ) -> Result<(), IndexError> {
        let claimed = sqlx::query(
            "INSERT INTO indexer_applied_events (event_id, shard_id) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING RETURNING event_id",
        )
        .bind(event.id)
        .bind(origin.map_or("", |o| o.shard_id.as_str()))
        .fetch_optional(&mut **tx)
        .await?;

        if claimed.is_none() {
            return Ok(());
        }

        // Proof comes before the identity chain: a refused event must not reach it either.
        let local_network = self.local_origin.as_ref().map(|o| o.network_id.as_str());
        let verified = identity_proof::verify(tx, event, origin, local_network).await?;

        // Chained events are recorded and resolved first; only `profile.updated`
        // is projected through the resolved chain (see `apply_profile_chained`).
        match identity_chain_store::record(tx, event, OffsetDateTime::now_utc()).await? {
            Recorded::Unchained => {}
            Recorded::Duplicate => return Ok(()),
            Recorded::Rejected(err) => {
                eprintln!("indexer: dropping chained event {}: {err:?}", event.id);
                return Ok(());
            }
            Recorded::Chained {
                accepted,
                displaced,
                newly_accepted,
                accepted_events,
            } => {
                if event.kind == "profile.updated" {
                    apply_profile_chained(
                        tx,
                        event,
                        accepted,
                        &displaced,
                        !newly_accepted.is_empty(),
                        &accepted_events,
                    )
                    .await?;
                    return Ok(());
                }
            }
        }

        match event.kind.as_str() {
            "identity.created" => {
                if let (Verified::Creation(created), Some(origin)) = (&verified, origin) {
                    profiles::apply_created(tx, event.timestamp, created, origin).await?;
                }
            }
            "profile.updated" => {
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
            // `avalon-docs/architecture/query-and-indexing.md`'s
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
                // avalon-docs/architecture/query-and-indexing.md.
                eprintln!("indexer: skipping unrecognized event kind {other:?}");
            }
        }

        Ok(())
    }
}

/// Projects a chained `profile.updated`: a rejected or superseded event is
/// skipped, and any change to which events are accepted (a winner displacing
/// an applied loser, or an out-of-order gap filling) reverts the displaced
/// events' fields and replays every accepted profile event in chain order, so
/// the resulting row is a function of the accepted set alone.
async fn apply_profile_chained(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
    accepted: bool,
    displaced: &[ProtocolEvent],
    gap_filled: bool,
    accepted_events: &[ProtocolEvent],
) -> Result<(), IndexError> {
    if displaced.is_empty() && !gap_filled {
        if accepted {
            if let Some(write) = profiles::decode(event) {
                profiles::apply(tx, &write).await?;
            }
        }
        return Ok(());
    }
    for old in displaced.iter().filter(|e| e.kind == "profile.updated") {
        if let Some(write) = profiles::decode(old) {
            profiles::apply(tx, &profiles::revert_of(&write)).await?;
        }
    }
    for ev in accepted_events
        .iter()
        .filter(|e| e.kind == "profile.updated")
    {
        if let Some(write) = profiles::decode(ev) {
            profiles::apply(tx, &write).await?;
        }
    }
    Ok(())
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
