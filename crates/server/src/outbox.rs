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
//!
//! **Remote-submit mode (issue #313).** When `AVALON_SETTLEMENT_REMOTE_URL`
//! is configured, a drain tick posts its `EventBatch` to `POST
//! /ledger/submit` on the named remote Settlement authority instead of
//! calling `chain.commit` against this node's own pool — the authority runs
//! the exact same `chain.commit` call for it that it runs for its own local
//! outbox, so there's still exactly one canonical committer. Unset (the
//! default), this worker behaves exactly as it always has — see
//! [`RemoteSubmitConfig::from_env`] and [`RemoteSubmitConfig::submit`].

use avalon_chain::{PostgresSettlementProvider, SettlementError, SettlementProvider};
use avalon_protocol::events::{Commitment, EventBatch, ProtocolEvent};
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
const DEFAULT_POLL_INTERVAL_SECS: u64 = 3;

/// Issue #363, implementing #287's decision: hoster-configurable drain
/// cadence, same default this constant always hardcoded.
fn poll_interval_from_env() -> std::time::Duration {
    let secs = std::env::var("AVALON_OUTBOX_POLL_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_POLL_INTERVAL_SECS);
    std::time::Duration::from_secs(secs)
}

/// Node-to-node config for posting drained batches to a remote Settlement
/// authority instead of committing them locally — issue #313. Bundles the
/// `reqwest::Client` alongside the target so `run_worker` only builds one.
pub struct RemoteSubmitConfig {
    client: reqwest::Client,
    /// Base URL of the remote Settlement authority, no trailing slash.
    url: String,
    /// `AVALON_SETTLEMENT_SUBMIT_KEY` — sent as a bearer credential on every
    /// submit. See `crate::settlement::submit_ledger_batch`'s doc comment
    /// for why this is the chosen node-to-node auth mechanism.
    submit_key: Option<String>,
}

impl RemoteSubmitConfig {
    /// `AVALON_SETTLEMENT_REMOTE_URL` unset (the default) returns `None`,
    /// leaving `run_worker` on its original local-commit path — this ticket
    /// must be purely additive. `AVALON_SETTLEMENT_SUBMIT_KEY` is read here
    /// too (sent as this node's own credential to the remote authority) but
    /// is optional at the type level; an authority with no submit key of
    /// its own configured refuses every submission regardless (see
    /// `crate::settlement::submit_ledger_batch`), so an operator who forgets
    /// it on one side simply gets every submission rejected, not silently
    /// unauthenticated.
    pub fn from_env() -> Option<Self> {
        let url = std::env::var("AVALON_SETTLEMENT_REMOTE_URL").ok()?;
        let url = url.trim().trim_end_matches('/').to_string();
        if url.is_empty() {
            return None;
        }
        let submit_key = std::env::var("AVALON_SETTLEMENT_SUBMIT_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        Some(Self {
            client: reqwest::Client::new(),
            url,
            submit_key,
        })
    }

    async fn submit(&self, batch: &EventBatch) -> Result<Commitment, SettlementError> {
        let mut request = self
            .client
            .post(format!("{}/ledger/submit", self.url))
            .json(batch);
        if let Some(key) = &self.submit_key {
            request = request.bearer_auth(key);
        }
        let response = request
            .send()
            .await
            .map_err(|e| SettlementError::Storage(format!("remote submit request failed: {e}")))?
            .error_for_status()
            .map_err(|e| {
                SettlementError::Storage(format!("remote submit rejected by authority: {e}"))
            })?;
        response.json::<Commitment>().await.map_err(|e| {
            SettlementError::Storage(format!("remote submit returned an unparseable body: {e}"))
        })
    }
}

/// Drains pending outbox rows, oldest first, forever — into `chain`
/// directly, or (issue #313) into a remote Settlement authority named by
/// `remote`. Spawned once at server startup (`main.rs`) as a background
/// task in the same process — not a separate binary or deployment. Never
/// returns; intended to be handed to `tokio::spawn`.
pub async fn run_worker(
    pool: PgPool,
    chain: PostgresSettlementProvider,
    remote: Option<RemoteSubmitConfig>,
) {
    let poll_interval = poll_interval_from_env();
    loop {
        if let Err(err) = drain_once(&pool, &chain, remote.as_ref()).await {
            tracing::error!("outbox worker: {err}");
        }
        tokio::time::sleep(poll_interval).await;
    }
}

async fn drain_once(
    pool: &PgPool,
    chain: &PostgresSettlementProvider,
    remote: Option<&RemoteSubmitConfig>,
) -> Result<(), sqlx::Error> {
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
            tracing::error!(%id, "outbox worker: dropping unparseable row");
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
    //
    // Issue #313: `remote` is `None` on every deployment that hasn't set
    // `AVALON_SETTLEMENT_REMOTE_URL` — `chain.commit` below is the exact
    // same call this worker has always made. Never both: exactly one
    // Settlement authority ever accepts writes for a network, so this is
    // either-or, never a local-then-remote fallback.
    let commit_result = match remote {
        None => chain.commit(&batch).await,
        Some(remote) => remote.submit(&batch).await,
    };
    match commit_result {
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
            tracing::error!("outbox worker: chain.commit failed, batch left pending: {err}");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outbox_poll_interval_env_var_overrides_default() {
        // SAFETY-of-intent note: `std::env::set_var` is process-global;
        // this test does not run concurrently with anything else reading
        // this exact var (no other test in this crate touches
        // `AVALON_OUTBOX_POLL_INTERVAL_SECS`), so it's safe here despite
        // being `unsafe` in edition-2024 terms.
        unsafe {
            std::env::set_var("AVALON_OUTBOX_POLL_INTERVAL_SECS", "7");
        }
        assert_eq!(poll_interval_from_env(), std::time::Duration::from_secs(7));
        unsafe {
            std::env::remove_var("AVALON_OUTBOX_POLL_INTERVAL_SECS");
        }
        assert_eq!(
            poll_interval_from_env(),
            std::time::Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS)
        );
    }

    #[test]
    fn a_non_positive_outbox_poll_interval_env_var_falls_back_to_default() {
        unsafe {
            std::env::set_var("AVALON_OUTBOX_POLL_INTERVAL_SECS", "0");
        }
        assert_eq!(
            poll_interval_from_env(),
            std::time::Duration::from_secs(DEFAULT_POLL_INTERVAL_SECS)
        );
        unsafe {
            std::env::remove_var("AVALON_OUTBOX_POLL_INTERVAL_SECS");
        }
    }
}
