//! The outbox pattern, closing issue #71.
//!
//! An app-data write and the durable protocol event it accompanies must
//! land together or not at all. Previously the app-data transaction
//! committed, then a *separate* call to `SettlementProvider::commit`
//! happened after the fact — a crash between the two left, e.g., an
//! identity with no corresponding ledger entry, silently breaking "every
//! identity has a durable creation record."
//!
//! The fix: [`enqueue`] writes the event into `protocol_outbox` inside the
//! *same* transaction as whatever app-data rows it describes, before any
//! external ledger call ever happens. [`run_worker`] then drains pending
//! rows into `avalon-chain` at its own pace. A row that fails to publish
//! immediately is not data loss — it is already durably recorded here, and
//! is retried on the worker's next tick.

use avalon_chain::{PostgresSettlementProvider, SettlementProvider};
use avalon_protocol::events::{EventBatch, ProtocolEvent};
use sqlx::{PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

/// Enqueues `event` as part of `tx`. Callers must be inside the same
/// transaction as the app-data rows this event describes — never call this
/// after a transaction has already committed, which would recreate exactly
/// the bug this module exists to close.
pub async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
) -> Result<(), sqlx::Error> {
    let event_json = serde_json::to_value(event).expect("ProtocolEvent should serialize");
    sqlx::query("INSERT INTO protocol_outbox (event) VALUES ($1)")
        .bind(event_json)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

const DRAIN_BATCH_SIZE: i64 = 20;
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);

/// Drains pending outbox rows into `chain`, oldest first, forever. Spawned
/// once at server startup (`main.rs`) as a background task in the same
/// process — not a separate binary or deployment. Never returns; intended
/// to be handed to `tokio::spawn`.
pub async fn run_worker(pool: PgPool, chain: PostgresSettlementProvider) {
    loop {
        if let Err(err) = drain_once(&pool, &chain).await {
            eprintln!("outbox worker: {err}");
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn drain_once(pool: &PgPool, chain: &PostgresSettlementProvider) -> Result<(), sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, event FROM protocol_outbox WHERE committed_at IS NULL ORDER BY enqueued_at LIMIT $1",
    )
    .bind(DRAIN_BATCH_SIZE)
    .fetch_all(pool)
    .await?;

    for row in rows {
        let id: Uuid = row.try_get("id")?;
        let event_json: serde_json::Value = row.try_get("event")?;

        let Ok(event) = serde_json::from_value::<ProtocolEvent>(event_json) else {
            // Should never happen — we only ever write valid ProtocolEvents
            // here. Mark it committed anyway rather than `continue`: an
            // unparseable row is always among the oldest pending (this
            // query is oldest-first), so silently skipping it would leave
            // it re-fetched, and re-failed, on every future tick forever —
            // starving every legitimate row behind it once there are more
            // than DRAIN_BATCH_SIZE pending. Losing one malformed event this
            // way is a far smaller failure than wedging the whole queue.
            eprintln!("outbox worker: dropping unparseable row {id}");
            sqlx::query("UPDATE protocol_outbox SET committed_at = now() WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await?;
            continue;
        };

        let batch = EventBatch {
            id: Uuid::new_v4(),
            events: vec![event],
            created_at: OffsetDateTime::now_utc(),
        };

        if chain.commit(&batch).await.is_ok() {
            sqlx::query("UPDATE protocol_outbox SET committed_at = now() WHERE id = $1")
                .bind(id)
                .execute(pool)
                .await?;
        }
        // Failure: leave it pending, retried next tick.
    }

    Ok(())
}

/// Pending-count and oldest-pending age, for `avalon outbox-status`.
pub struct OutboxStatus {
    pub pending_count: i64,
    pub oldest_pending: Option<OffsetDateTime>,
}

pub async fn status(pool: &PgPool) -> Result<OutboxStatus, sqlx::Error> {
    let row = sqlx::query(
        "SELECT count(*) AS pending_count, min(enqueued_at) AS oldest_pending \
         FROM protocol_outbox WHERE committed_at IS NULL",
    )
    .fetch_one(pool)
    .await?;
    Ok(OutboxStatus {
        pending_count: row.try_get("pending_count")?,
        oldest_pending: row.try_get("oldest_pending")?,
    })
}
