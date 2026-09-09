//! `PostgresIndexer` — the first real [`Indexer`], closing issue #42.
//!
//! Dispatches by `event.kind` to the matching module under
//! [`crate::projections`], guarded by an `indexer_applied_events(event_id)`
//! dedup table so a redelivered or replayed (rebuild, #43) event is a no-op
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

use crate::projections::{attestations, friendships, guild_rosters, profiles};
use crate::{IndexError, Indexer};

#[derive(Clone)]
pub struct PostgresIndexer {
    pool: PgPool,
}

impl PostgresIndexer {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
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
            "friend.requested" | "friend.accepted" | "friend.removed" => {
                if let Some(write) = friendships::decode(event) {
                    friendships::apply(tx, &write).await?;
                }
            }
            "guild.member_added" | "guild.member_removed" | "guild.role_changed" => {
                if let Some(write) = guild_rosters::decode(event) {
                    guild_rosters::apply(tx, &write).await?;
                }
            }
            "achievement.issued" | "achievement.revoked" => {
                if let Some(write) = attestations::decode(event) {
                    attestations::apply(tx, &write).await?;
                }
            }
            other => {
                // Never an error — an old indexer must survive a new event
                // kind being introduced elsewhere in the protocol. See
                // docs/architecture/query-and-indexing.md.
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
