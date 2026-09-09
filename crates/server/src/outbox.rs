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
//!
//! Batching (issue #38): a drain tick groups every row it picks up into one
//! `EventBatch` and commits it with a single `SettlementProvider::commit`
//! call, not one call per event — a batch closes whenever the worker runs;
//! a batch of one event is legal (an idle system draining a single pending
//! row). Rows are marked with the `batch_id` the commit actually produced,
//! not left `NULL` — the column has been reserved for this since #71's
//! `0003_outbox` migration.

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

    // One EventBatch for this whole tick's rows, not one per row — issue
    // #38: a protocol event is never its own settlement action.
    let mut pending_ids = Vec::with_capacity(rows.len());
    let mut events = Vec::with_capacity(rows.len());

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

        pending_ids.push(id);
        events.push(event);
    }

    if events.is_empty() {
        return Ok(());
    }

    let batch = EventBatch {
        id: Uuid::new_v4(),
        events,
        created_at: OffsetDateTime::now_utc(),
    };

    // Failure: leave every row in this tick's batch pending, retried whole
    // (as a new batch) next tick — `commit` is one transaction, so nothing
    // was partially settled. Still logged, though — a `commit` that fails
    // every tick (e.g. a missing signing key) must not fail silently
    // forever; the pending rows alone don't say why they're stuck.
    match chain.commit(&batch).await {
        Ok(commitment) => {
            for id in pending_ids {
                sqlx::query(
                    "UPDATE protocol_outbox SET committed_at = now(), batch_id = $2 WHERE id = $1",
                )
                .bind(id)
                .bind(commitment.batch_id)
                .execute(pool)
                .await?;
            }
        }
        Err(err) => {
            eprintln!("outbox worker: chain.commit failed, batch left pending: {err}");
        }
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
