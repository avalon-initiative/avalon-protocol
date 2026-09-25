//! The outbox pattern.
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
//! Batching: a drain tick groups every row it picks up into one
//! `EventBatch` and commits it with a single `SettlementProvider::commit`
//! call, not one call per event — a batch closes whenever the worker runs;
//! a batch of one event is legal (an idle system draining a single pending
//! row). Rows are marked with the `batch_id` the commit actually produced,
//! not left `NULL` — the column has been reserved for this since the
//! `0003_outbox` migration.
//!
//! **Remote-submit mode.** When `AVALON_SETTLEMENT_REMOTE_URL`
//! is configured, a drain tick posts its `EventBatch` to `POST
//! /ledger/submit` on the named remote Settlement authority instead of
//! calling `chain.commit` against this node's own pool — the authority runs
//! the exact same `chain.commit` call for it that it runs for its own local
//! outbox, so there's still exactly one canonical committer. Unset (the
//! default), this worker behaves exactly as it always has — see
//! [`RemoteSubmitConfig::from_env`] and [`RemoteSubmitConfig::submit`].
//!
//! **Shard-aware routing, implementing the decided sharded
//! settlement design.** A single drain tick can now span more than one shard's
//! worth of pending rows — [`shard_id_for_event`] derives which shard an
//! event belongs to from its `issuer`'s [`GlobalId`] namespace (`game`/
//! `app`/`service` routes to that integrator's own shard; everything else
//! — identity/social/guild events, which have no single owning integrator
//! — routes to one reserved `"core"` shard). [`drain_locked`] groups
//! pending rows by that derived `shard_id`, builds **one `EventBatch` per
//! shard per tick** (never mixing two shards' events into one batch), and
//! commits each independently: locally via `chain.commit` if this node
//! holds no configured remote target for that shard, or via
//! [`RemoteSubmitConfig`] to that shard's own configured authority
//! otherwise. `AVALON_SETTLEMENT_REMOTE_URL` (singular) keeps working
//! completely unchanged — it's exactly `AVALON_SETTLEMENT_REMOTE_URLS`'s
//! implicit `core=<url>` entry, so milestone-1's single-shard topology
//! needs zero configuration change. See
//! `docs/projects/backend-server/architecture/settlement.md`'s "Write routing to the correct
//! shard" section for the full design.
//!
//! **Push-based mirror sync.** Right after a batch commits
//! *locally* (never after a remote submit — that authority's own outbox
//! fires its own push when it commits), [`drain_locked`] fetches this
//! node's fresh `checkpoint()` and calls
//! `crate::mirror_push::notify_peers` with it. This is the one and only
//! place a new STH is announced to interested mirrors — see
//! `crate::mirror_push`'s own module doc comment for the addressing/
//! delivery/trust design. `mirror_push` being `None` (no DHT identity —
//! `AVALON_DHT_ENABLED` unset) makes this call site a no-op, exactly
//! `outbox`'s behavior before #596 existed.

use std::collections::BTreeMap;

use avalon_chain::{PostgresSettlementProvider, SettlementError, SettlementProvider};
use avalon_protocol::events::{Commitment, EventBatch, ProtocolEvent};
use sqlx::{PgPool, Postgres, Row, Transaction};
use time::OffsetDateTime;
use uuid::Uuid;

/// Issue #532: which shard `event` belongs to, derived from its `issuer`'s
/// `GlobalId` namespace (`<namespace>:<owner>:<kind>:<key>`) — never from
/// any other field, since `issuer` is exactly "who authored this event,"
/// the same question shard authority answers. `game`/`app`/`service` (an
/// integrator acting as itself) routes to that integrator's own shard,
/// `"{namespace}:{owner}"`. Everything else — `identity` namespace events
/// (identity/social-graph/guild events; a friendship or guild isn't owned
/// by any one integrator) — routes to the one reserved `"core"` shard,
/// not split further. A milestone-1 deployment with no
/// `AVALON_SETTLEMENT_REMOTE_URLS` configured has exactly one shard
/// (`"core"`) and behaves exactly as today.
pub(crate) fn shard_id_for_event(event: &ProtocolEvent) -> String {
    let issuer = event.issuer.as_str();
    let mut parts = issuer.splitn(3, ':');
    let namespace = parts.next().unwrap_or("");
    let owner = parts.next().unwrap_or("");
    match namespace {
        "game" | "app" | "service" => format!("{namespace}:{owner}"),
        _ => "core".to_string(),
    }
}

/// Enqueues `event` as part of `tx`. Callers must be inside the same
/// transaction as the app-data rows this event describes — never call this
/// after a transaction has already committed, which would recreate exactly
/// the bug this module exists to close.
pub async fn enqueue(
    tx: &mut Transaction<'_, Postgres>,
    event: &ProtocolEvent,
) -> Result<(), sqlx::Error> {
    let event_json = serde_json::to_value(event).expect("ProtocolEvent should serialize");
    if let Some(trace) = crate::op_trace::pending_for_current() {
        let row_id: Uuid =
            sqlx::query_scalar("INSERT INTO protocol_outbox (event) VALUES ($1) RETURNING id")
                .bind(event_json)
                .fetch_one(&mut **tx)
                .await?;
        crate::op_trace::register_pending(row_id, trace);
        return Ok(());
    }
    sqlx::query("INSERT INTO protocol_outbox (event) VALUES ($1)")
        .bind(event_json)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

const DRAIN_BATCH_SIZE: i64 = 20;
const DEFAULT_POLL_INTERVAL_SECS: u64 = 3;

/// A remote submit with no bound at all can hang a whole drain tick
/// forever: `drain_locked` awaits each shard's commit in sequence while
/// still holding `OUTBOX_DRAIN_LOCK_KEY`, so one unresponsive (not just
/// erroring — genuinely hung) remote authority wedges every shard's
/// pending rows, not just its own, and blocks every later tick from even
/// acquiring the lock. Same 10s convention as
/// `internal_role::REMOTE_INDEXER_TIMEOUT`.
const REMOTE_SETTLEMENT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

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

/// Issue #526: one shard's currently-ongoing remote-submit failure — this
/// node is configured to forward that shard's writes to `authority_url`,
/// but the most recent attempt failed with `reason`. Cleared (removed from
/// [`RemoteSubmitStatus`]) the moment a submit to that shard next
/// succeeds — this is "currently failing," not a permanent failure log.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RemoteFailure {
    pub authority_url: String,
    pub reason: String,
    #[serde(with = "time::serde::rfc3339")]
    pub since: OffsetDateTime,
}

/// Issue #526: shared, in-process record of which shards (if any) are
/// currently failing to reach their configured remote Settlement
/// authority — written by [`drain_locked`] on every remote submit
/// attempt, read by `crate::settlement::remote_submit_status` to answer
/// `GET /ledger/remote-submit-status`. A forwarding node's integrator
/// otherwise has no way to discover the correct authority from a failure
/// alone — this makes the node's own already-known target discoverable
/// from a client-visible response instead of only from server logs.
/// Never proposes an alternate authority: every entry's `authority_url` is
/// exactly what this node was already configured with for that shard.
#[derive(Clone, Default)]
pub struct RemoteSubmitStatus {
    failing: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<String, RemoteFailure>>>,
}

impl RemoteSubmitStatus {
    fn record_failure(&self, shard_id: &str, authority_url: &str, reason: String) {
        let mut failing = self
            .failing
            .lock()
            .expect("RemoteSubmitStatus lock poisoned");
        let entry = failing
            .entry(shard_id.to_string())
            .or_insert_with(|| RemoteFailure {
                authority_url: authority_url.to_string(),
                reason: reason.clone(),
                since: OffsetDateTime::now_utc(),
            });
        entry.reason = reason;
    }

    fn record_success(&self, shard_id: &str) {
        self.failing
            .lock()
            .expect("RemoteSubmitStatus lock poisoned")
            .remove(shard_id);
    }

    /// Every shard currently failing to reach its configured remote
    /// authority, `shard_id` alongside its [`RemoteFailure`]. Empty when
    /// every configured shard's most recent submit succeeded (or none is
    /// configured at all).
    pub fn currently_failing(&self) -> Vec<(String, RemoteFailure)> {
        self.failing
            .lock()
            .expect("RemoteSubmitStatus lock poisoned")
            .iter()
            .map(|(shard_id, failure)| (shard_id.clone(), failure.clone()))
            .collect()
    }
}

/// Node-to-node config for posting drained batches to a remote Settlement
/// authority instead of committing them locally — issue #313, extended to
/// a per-shard map by issue #532. Bundles the `reqwest::Client` alongside
/// the targets so `run_worker` only builds one.
pub struct RemoteSubmitConfig {
    client: reqwest::Client,
    /// `shard_id` -> base URL of that shard's remote Settlement authority
    /// (no trailing slash). A shard with no entry here is committed
    /// locally via `chain.commit` — this node is presumed authoritative
    /// for any shard it hasn't been told to defer.
    targets: std::collections::HashMap<String, String>,
    /// `AVALON_SETTLEMENT_SUBMIT_KEY` — sent as a bearer credential on every
    /// submit, to every configured target. See
    /// `crate::settlement::submit_ledger_batch`'s doc comment for why this
    /// is the chosen node-to-node auth mechanism.
    submit_key: Option<String>,
    /// Issue #526 — see [`RemoteSubmitStatus`]'s own doc comment.
    status: RemoteSubmitStatus,
}

impl RemoteSubmitConfig {
    /// Both `AVALON_SETTLEMENT_REMOTE_URL` and `AVALON_SETTLEMENT_REMOTE_URLS`
    /// unset (the default) returns `None`, leaving `run_worker` on its
    /// original local-commit path for every shard — this ticket must be
    /// purely additive, matching #313's own invariant.
    ///
    /// `AVALON_SETTLEMENT_REMOTE_URLS` — issue #532 — is a comma-separated
    /// `shard_id=url` list (e.g. `game:ashen-realms=https://a.example,core=https://core.example`),
    /// one remote authority per shard. `AVALON_SETTLEMENT_REMOTE_URL`
    /// (singular, #313's original var) keeps working completely
    /// unchanged: it's merged in as the implicit `core=<url>` entry
    /// whenever `AVALON_SETTLEMENT_REMOTE_URLS` doesn't already name
    /// `core` itself — a milestone-1 deployment with only the singular
    /// var set behaves byte-for-byte as it always has, just expressed
    /// through the same per-shard map every other shard now uses too.
    ///
    /// `AVALON_SETTLEMENT_SUBMIT_KEY` is read here too (sent as this
    /// node's own credential to every configured remote authority) but is
    /// optional at the type level; an authority with no submit key of its
    /// own configured refuses every submission regardless (see
    /// `crate::settlement::submit_ledger_batch`), so an operator who
    /// forgets it on one side simply gets every submission rejected, not
    /// silently unauthenticated.
    pub fn from_env() -> Option<Self> {
        let mut targets = std::collections::HashMap::new();

        if let Ok(raw) = std::env::var("AVALON_SETTLEMENT_REMOTE_URLS") {
            for entry in raw.split(',') {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                let Some((shard_id, url)) = entry.split_once('=') else {
                    tracing::error!(
                        entry,
                        "outbox: AVALON_SETTLEMENT_REMOTE_URLS entry missing '=' — ignoring"
                    );
                    continue;
                };
                let shard_id = shard_id.trim().to_string();
                if shard_id.is_empty() || url.trim().is_empty() {
                    continue;
                }
                // Issue #665: same well-formedness check
                // `nodes::realtime_mode_from_env`/`internal_role::RemoteIndexer::from_env`
                // use for their own backing-service URLs — a malformed
                // per-shard entry is logged and skipped (this var's
                // existing "ignore this one entry" posture, unlike
                // Indexer/Realtime's hard failure — see
                // `crate::backing_services`'s own doc comment for why),
                // rather than silently used as-is and only failing later
                // on the first real submit attempt.
                match crate::backing_services::normalize_and_validate_url(
                    "AVALON_SETTLEMENT_REMOTE_URLS",
                    url,
                ) {
                    Ok(url) => {
                        targets.insert(shard_id, url);
                    }
                    Err(e) => tracing::error!(
                        entry,
                        "outbox: AVALON_SETTLEMENT_REMOTE_URLS entry malformed — ignoring: {e}"
                    ),
                }
            }
        }

        if let Ok(url) = std::env::var("AVALON_SETTLEMENT_REMOTE_URL") {
            if !url.trim().is_empty() {
                match crate::backing_services::normalize_and_validate_url(
                    "AVALON_SETTLEMENT_REMOTE_URL",
                    &url,
                ) {
                    Ok(url) => {
                        targets.entry("core".to_string()).or_insert(url);
                    }
                    Err(e) => tracing::error!(
                        "outbox: AVALON_SETTLEMENT_REMOTE_URL malformed — ignoring: {e}"
                    ),
                }
            }
        }

        if targets.is_empty() {
            return None;
        }

        let submit_key = std::env::var("AVALON_SETTLEMENT_SUBMIT_KEY")
            .ok()
            .filter(|s| !s.is_empty());
        Some(Self {
            client: reqwest::Client::builder()
                .timeout(REMOTE_SETTLEMENT_TIMEOUT)
                .build()
                .expect("reqwest client with only a timeout set should always build"),
            targets,
            submit_key,
            status: RemoteSubmitStatus::default(),
        })
    }

    /// Issue #526: a cheap-to-clone handle to this config's live
    /// failure-tracking state, for `AppState` to hold and
    /// `crate::settlement::remote_submit_status` to read — see
    /// [`RemoteSubmitStatus`]'s own doc comment.
    pub fn status(&self) -> RemoteSubmitStatus {
        self.status.clone()
    }

    /// This shard's configured remote authority, if any — `None` means
    /// this node commits that shard's batches locally.
    fn target_for_shard(&self, shard_id: &str) -> Option<&str> {
        self.targets.get(shard_id).map(String::as_str)
    }

    /// Every configured `(shard_id, base_url)` pair — read by `main.rs`'s
    /// issue #665 startup reachability check, so it doesn't need its own
    /// second parse of `AVALON_SETTLEMENT_REMOTE_URL(S)`.
    pub fn targets(&self) -> &std::collections::HashMap<String, String> {
        &self.targets
    }

    async fn submit(
        &self,
        url: &str,
        batch: &EventBatch,
        trace: Option<&crate::op_trace::PendingTrace>,
    ) -> Result<Commitment, SettlementError> {
        use crate::op_trace::{
            branches_for_forward, from_response, store_result, Downstream, OpTrace,
        };
        let mut request = self.client.post(format!("{url}/ledger/submit")).json(batch);
        if let Some(key) = &self.submit_key {
            request = request.bearer_auth(key);
        }
        if let Some(t) = trace {
            request = request.header(crate::op_trace::TRACE_HEADER, t.trace_id.to_string());
        }
        let sent = std::time::Instant::now();
        let sent_result = request.send().await;
        if let Some(t) = trace {
            let down = match &sent_result {
                Ok(r) if r.status().is_success() => {
                    Downstream::Answered(from_response(r.headers(), t.trace_id))
                }
                Ok(_) => Downstream::Answered(None),
                Err(e) if e.is_timeout() => Downstream::Timeout,
                Err(_) => Downstream::Unreachable,
            };
            let (branches, truncated) = branches_for_forward(
                &t.identity,
                sent.duration_since(t.queued),
                sent.elapsed(),
                url,
                down,
            );
            store_result(OpTrace {
                trace_id: t.trace_id,
                branches,
                truncated,
            });
        }
        let response = sent_result
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
/// directly, or into a remote Settlement authority named by
/// `remote`. Spawned once at server startup (`main.rs`) as a background
/// task in the same process — not a separate binary or deployment. Never
/// returns; intended to be handed to `tokio::spawn`.
pub async fn run_worker(
    pool: PgPool,
    chain: PostgresSettlementProvider,
    remote: Option<RemoteSubmitConfig>,
    mirror_push: Option<crate::mirror_push::MirrorPushConfig>,
) {
    let poll_interval = poll_interval_from_env();
    loop {
        if let Err(err) = drain_once(&pool, &chain, remote.as_ref(), mirror_push.as_ref()).await {
            tracing::error!("outbox worker: {err}");
        }
        tokio::time::sleep(poll_interval).await;
    }
}

/// Postgres advisory lock key guarding a drain tick, issue #536: without
/// it, more than one `avalon-server` process polling the same
/// `protocol_outbox` table can select the same pending rows and both
/// commit them into the ledger — a silent double-commit, not just a race
/// on who commits first. Session-scoped (tied to the connection that took
/// it), held only for the duration of one tick, and released even if this
/// process crashes mid-tick (Postgres drops session-scoped advisory locks
/// when the holding connection closes).
const OUTBOX_DRAIN_LOCK_KEY: i64 = 53_602;

async fn drain_once(
    pool: &PgPool,
    chain: &PostgresSettlementProvider,
    remote: Option<&RemoteSubmitConfig>,
    mirror_push: Option<&crate::mirror_push::MirrorPushConfig>,
) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;

    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(OUTBOX_DRAIN_LOCK_KEY)
        .fetch_one(&mut *conn)
        .await?;
    if !acquired {
        // Another process (or another tick, if a prior one somehow hung)
        // is already draining — skip this tick rather than blocking, and
        // try again on the next poll.
        return Ok(());
    }

    let result = drain_locked(&mut conn, chain, remote, mirror_push).await;

    let _: bool = sqlx::query_scalar("SELECT pg_advisory_unlock($1)")
        .bind(OUTBOX_DRAIN_LOCK_KEY)
        .fetch_one(&mut *conn)
        .await?;

    result
}

async fn drain_locked(
    conn: &mut sqlx::PgConnection,
    chain: &PostgresSettlementProvider,
    remote: Option<&RemoteSubmitConfig>,
    mirror_push: Option<&crate::mirror_push::MirrorPushConfig>,
) -> Result<(), sqlx::Error> {
    let rows = sqlx::query(
        "SELECT id, event FROM protocol_outbox WHERE committed_at IS NULL ORDER BY enqueued_at LIMIT $1",
    )
    .bind(DRAIN_BATCH_SIZE)
    .fetch_all(&mut *conn)
    .await?;

    // Grouped by shard, not one flat list — a protocol event
    // is never its own settlement action, but two different shards'
    // events are never allowed into the same batch either, since a batch
    // closes under exactly one shard's authority. `BTreeMap` (not
    // `HashMap`) purely so shard processing order is deterministic run to
    // run, which makes this worker's own logs/tests reproducible — the
    // order shards are committed in has no correctness meaning.
    let mut by_shard: BTreeMap<String, (Vec<Uuid>, Vec<ProtocolEvent>)> = BTreeMap::new();

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
                .execute(&mut *conn)
                .await?;
            continue;
        };

        let shard_id = shard_id_for_event(&event);
        let group = by_shard.entry(shard_id).or_default();
        group.0.push(id);
        group.1.push(event);
    }

    for (shard_id, (pending_ids, events)) in by_shard {
        if events.is_empty() {
            continue;
        }

        let batch = EventBatch {
            id: Uuid::new_v4(),
            events,
            created_at: OffsetDateTime::now_utc(),
        };

        // Failure: leave every row in this shard's batch pending, retried
        // whole (as a new batch) next tick — `commit` is one transaction,
        // so nothing was partially settled. A failure on one shard never
        // blocks another shard's batch this same tick — each is fully
        // independent. Still logged, though — a `commit` that fails every
        // tick (e.g. a missing signing key) must not fail silently
        // forever; the pending rows alone don't say why they're stuck.
        //
        // Issue #313/#532: no configured remote target for this shard
        // means this node commits it locally — `chain.commit` below is
        // the exact same call this worker has always made for every
        // shard before #532 existed. Never both local and remote for the
        // same shard: exactly one Settlement authority ever accepts
        // writes for a given shard, so this is either-or per shard, never
        // a local-then-remote fallback.
        let remote_target = remote.and_then(|r| r.target_for_shard(&shard_id).map(|url| (r, url)));
        let commit_result = match remote_target {
            None => chain.commit(&batch).await,
            Some((remote, url)) => {
                let trace = crate::op_trace::find_pending(&pending_ids);
                remote.submit(url, &batch, trace.as_ref()).await
            }
        };
        match commit_result {
            Ok(commitment) => {
                crate::op_trace::clear_pending(&pending_ids);
                // Issue #526: a shard that just succeeded is no longer
                // "currently failing" — clear any stale record from an
                // earlier tick's outage. Local commits (`remote_target`
                // is `None`) have no remote authority to track at all.
                if let Some((remote, _)) = remote_target {
                    remote.status.record_success(&shard_id);
                }
                for id in pending_ids {
                    sqlx::query(
                        "UPDATE protocol_outbox SET committed_at = now(), batch_id = $2 WHERE id = $1",
                    )
                    .bind(id)
                    .bind(commitment.batch_id)
                    .execute(&mut *conn)
                    .await?;
                }

                // Issue #596: only for a batch this node itself just
                // committed locally — a remote-submit's authority fires
                // its own push from its own outbox tick when it commits,
                // so pushing here too would just be a second, redundant
                // notification for the same STH. `checkpoint()` re-reads
                // the STH `commit` just signed/stored rather than
                // threading `tree_size` back out of `Commitment` itself
                // (which is also the wire shape `POST /ledger/submit`
                // returns, so widening it would touch more than this one
                // call site for no real benefit).
                if remote_target.is_none() {
                    if let Some(mirror_push) = mirror_push {
                        match chain.checkpoint().await {
                            Ok(Some(sth)) => {
                                crate::mirror_push::notify_peers(
                                    mirror_push,
                                    chain.network_id(),
                                    sth.tree_size,
                                )
                                .await;
                            }
                            Ok(None) => {
                                // Should never happen — a commit just
                                // succeeded, so a checkpoint must exist.
                                // Not fatal either way: the next tick's
                                // commit (or the poll fallback) still
                                // covers it.
                                tracing::warn!(
                                    "outbox worker: commit succeeded but checkpoint() found no \
                                     STH — skipping this tick's push notification"
                                );
                            }
                            Err(err) => {
                                tracing::warn!(
                                    "outbox worker: failed to read checkpoint for push \
                                     notification, relying on peers' poll fallback: {err}"
                                );
                            }
                        }
                    }
                }
            }
            Err(err) => {
                // Issue #526: only a *remote* failure is worth tracking
                // for discovery — a local `chain.commit` failure has no
                // "authority URL" to surface, and would defeat the
                // "never proposes an alternate authority" invariant if it
                // did (there is no other authority; this node just failed
                // its own local commit).
                if let Some((remote, url)) = remote_target {
                    remote
                        .status
                        .record_failure(&shard_id, url, err.to_string());
                }
                tracing::error!(
                    shard_id,
                    "outbox worker: chain.commit failed, batch left pending: {err}"
                );
            }
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
    use avalon_protocol::ids::GlobalId;

    /// **Both live tests in this module call `drain_once` directly
    /// against the real, shared `protocol_outbox`/`OUTBOX_DRAIN_LOCK_KEY`
    /// — run them with `--test-threads=1`.** Two of these tests racing
    /// concurrently (cargo's default within one binary) can have one
    /// test's `drain_once` legitimately lose the advisory-lock race to
    /// the *other* test's concurrent call and skip its own tick, which
    /// looks like a flake (a row briefly not yet committed) but isn't a
    /// bug in `drain_once` itself — it's exactly two independent tests
    /// sharing one global resource without serializing against each
    /// other. `cargo test -p avalon-server --lib outbox -- --ignored
    /// --test-threads=1`.
    ///
    /// Issue #536: two `drain_once` calls racing against the same
    /// `protocol_outbox` rows must not both commit them into the ledger.
    /// Gated `--ignored`/live like every other test in this crate that
    /// touches real Postgres/`avalon-chain` (see `crates/chain/tests/settlement.rs`).
    /// Reproduces the bug directly against `drain_once` (real
    /// `PgPool`/`PostgresSettlementProvider`), not through the outer
    /// `run_worker` loop or HTTP — this is the exact race that existed
    /// before the advisory lock: no `--ignored` marker changes what's
    /// exercised, only whether it needs live infra to run.
    #[tokio::test]
    #[ignore]
    async fn concurrent_drains_do_not_double_commit() {
        avalon_devenv::load();
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect(&database_url)
            .await
            .expect("failed to connect to Postgres — is it reachable?");

        let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

        let mut tx = pool.begin().await.expect("begin failed");
        let mut event_ids = Vec::new();
        for i in 0..5 {
            let actor = Uuid::new_v4();
            let event = ProtocolEvent {
                id: Uuid::new_v4(),
                kind: "test.outbox_lock".to_string(),
                issuer: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
                subject: GlobalId::new("identity", &actor.to_string(), "self", "test_event"),
                payload: serde_json::json!({ "n": i }),
                timestamp: OffsetDateTime::now_utc(),
                version: 1,
                identity_chain: None,
            };
            event_ids.push(event.id);
            enqueue(&mut tx, &event).await.expect("enqueue failed");
        }
        tx.commit().await.expect("commit failed");

        // Before #536's fix, both of these would race past the (then
        // nonexistent) mutual exclusion, both `SELECT` the same pending
        // rows, and both call `chain.commit` with them — one ledger entry
        // per event per racing drain instead of one, total.
        let (r1, r2) = tokio::join!(
            drain_once(&pool, &chain, None, None),
            drain_once(&pool, &chain, None, None),
        );
        r1.expect("drain 1 failed");
        r2.expect("drain 2 failed");

        // One of the two calls above may have lost the advisory-lock race
        // to this environment's `make start` server (which runs the same
        // outbox worker against the same shared Postgres) and skipped its
        // own tick — a brief poll covers that legitimate case without
        // weakening the actual assertion below (still exactly 1, never
        // more).
        for id in &event_ids {
            let mut count = 0i64;
            for _ in 0..20 {
                count =
                    sqlx::query_scalar("SELECT count(*) FROM ledger_entries WHERE event_id = $1")
                        .bind(id)
                        .fetch_one(&pool)
                        .await
                        .expect("query failed");
                if count >= 1 {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            assert_eq!(count, 1, "event {id} was committed more than once");
        }
    }

    #[test]
    fn game_namespace_routes_to_its_own_shard() {
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "achievement.issued".to_string(),
            issuer: GlobalId::new("game", "ashen-realms", "self", "achievement_issued"),
            subject: GlobalId::new("identity", &Uuid::new_v4().to_string(), "self", "x"),
            payload: serde_json::json!({}),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
            identity_chain: None,
        };
        assert_eq!(shard_id_for_event(&event), "game:ashen-realms");
    }

    #[test]
    fn app_and_service_namespaces_also_route_to_their_own_shard() {
        for namespace in ["app", "service"] {
            let event = ProtocolEvent {
                id: Uuid::new_v4(),
                kind: "milestone.issued".to_string(),
                issuer: GlobalId::new(namespace, "some-integrator", "self", "milestone_issued"),
                subject: GlobalId::new("identity", &Uuid::new_v4().to_string(), "self", "x"),
                payload: serde_json::json!({}),
                timestamp: OffsetDateTime::now_utc(),
                version: 1,
                identity_chain: None,
            };
            assert_eq!(
                shard_id_for_event(&event),
                format!("{namespace}:some-integrator")
            );
        }
    }

    #[test]
    fn identity_namespace_routes_to_the_core_shard() {
        let actor = Uuid::new_v4();
        let event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "identity.created".to_string(),
            issuer: GlobalId::new("identity", &actor.to_string(), "self", "identity_created"),
            subject: GlobalId::new("identity", &actor.to_string(), "self", "identity_created"),
            payload: serde_json::json!({}),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
            identity_chain: None,
        };
        assert_eq!(shard_id_for_event(&event), "core");
    }

    /// Issue #532: a single drain tick spanning both a `game:...`-shard
    /// event and a `core`-shard event must produce **two** distinct
    /// `ledger_entries.batch_id` values, not one combined batch — direct
    /// live proof `drain_locked` actually groups by shard rather than
    /// still treating every pending row as one flat batch.
    #[tokio::test]
    #[ignore]
    async fn a_tick_spanning_two_shards_commits_two_separate_batches() {
        avalon_devenv::load();
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect(&database_url)
            .await
            .expect("failed to connect to Postgres — is it reachable?");

        let chain = PostgresSettlementProvider::new(pool.clone(), "avalon-test");

        let mut tx = pool.begin().await.expect("begin failed");

        let core_actor = Uuid::new_v4();
        let core_event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "test.shard_routing_core".to_string(),
            issuer: GlobalId::new(
                "identity",
                &core_actor.to_string(),
                "self",
                "shard_routing_test",
            ),
            subject: GlobalId::new(
                "identity",
                &core_actor.to_string(),
                "self",
                "shard_routing_test",
            ),
            payload: serde_json::json!({}),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
            identity_chain: None,
        };

        let integrator_slug = format!("shard-routing-test-{}", Uuid::new_v4().simple());
        let game_event = ProtocolEvent {
            id: Uuid::new_v4(),
            kind: "test.shard_routing_game".to_string(),
            issuer: GlobalId::new("game", &integrator_slug, "self", "shard_routing_test"),
            subject: GlobalId::new(
                "identity",
                &Uuid::new_v4().to_string(),
                "self",
                "shard_routing_test",
            ),
            payload: serde_json::json!({}),
            timestamp: OffsetDateTime::now_utc(),
            version: 1,
            identity_chain: None,
        };

        enqueue(&mut tx, &core_event).await.expect("enqueue failed");
        enqueue(&mut tx, &game_event).await.expect("enqueue failed");
        tx.commit().await.expect("commit failed");

        // Poll rather than assume this call's own `drain_once` wins the
        // advisory-lock race — this environment's `make start` server
        // runs its own outbox worker against the same shared Postgres, so
        // a tick can legitimately lose that race and skip (#536's own
        // documented, correct behavior), retried on the next poll rather
        // than a bug.
        for _ in 0..20 {
            let _ = drain_once(&pool, &chain, None, None).await;
            let both_committed: i64 =
                sqlx::query_scalar("SELECT count(*) FROM ledger_entries WHERE event_id = ANY($1)")
                    .bind([core_event.id, game_event.id])
                    .fetch_one(&pool)
                    .await
                    .expect("query failed");
            if both_committed == 2 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        let core_batch_id: Uuid =
            sqlx::query_scalar("SELECT batch_id FROM ledger_entries WHERE event_id = $1")
                .bind(core_event.id)
                .fetch_one(&pool)
                .await
                .expect("core event never committed within the timeout");
        let game_batch_id: Uuid =
            sqlx::query_scalar("SELECT batch_id FROM ledger_entries WHERE event_id = $1")
                .bind(game_event.id)
                .fetch_one(&pool)
                .await
                .expect("game event never committed within the timeout");

        assert_ne!(
            core_batch_id, game_batch_id,
            "a core-shard event and a game-shard event landed in the same batch — \
             shard grouping did not actually happen"
        );
    }

    #[test]
    fn outbox_poll_interval_env_var_overrides_default() {
        let _env = crate::test_env::guard();
        // Env vars are process-global; `test_env::guard` serializes every test that mutates them.
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
        let _env = crate::test_env::guard();
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

    /// Clears every remote-submit env var; call with `test_env::guard` held.
    fn clear_remote_submit_env_vars() {
        unsafe {
            std::env::remove_var("AVALON_SETTLEMENT_REMOTE_URL");
            std::env::remove_var("AVALON_SETTLEMENT_REMOTE_URLS");
            std::env::remove_var("AVALON_SETTLEMENT_SUBMIT_KEY");
        }
    }

    #[test]
    fn no_remote_env_vars_set_returns_none() {
        let _env = crate::test_env::guard();
        clear_remote_submit_env_vars();
        assert!(RemoteSubmitConfig::from_env().is_none());
    }

    #[test]
    fn singular_remote_url_becomes_the_implicit_core_entry() {
        let _env = crate::test_env::guard();
        clear_remote_submit_env_vars();
        unsafe {
            std::env::set_var("AVALON_SETTLEMENT_REMOTE_URL", "https://authority.example/");
        }
        let config = RemoteSubmitConfig::from_env().expect("should be Some");
        assert_eq!(
            config.target_for_shard("core"),
            Some("https://authority.example")
        );
        assert_eq!(config.target_for_shard("game:ashen-realms"), None);
        clear_remote_submit_env_vars();
    }

    #[test]
    fn plural_remote_urls_parses_a_per_shard_map() {
        let _env = crate::test_env::guard();
        clear_remote_submit_env_vars();
        unsafe {
            std::env::set_var(
                "AVALON_SETTLEMENT_REMOTE_URLS",
                "game:ashen-realms=https://a.example, core = https://core.example/",
            );
        }
        let config = RemoteSubmitConfig::from_env().expect("should be Some");
        assert_eq!(
            config.target_for_shard("game:ashen-realms"),
            Some("https://a.example")
        );
        assert_eq!(
            config.target_for_shard("core"),
            Some("https://core.example")
        );
        assert_eq!(config.target_for_shard("app:some-app"), None);
        clear_remote_submit_env_vars();
    }

    #[test]
    fn plural_remote_urls_naming_core_wins_over_the_singular_fallback() {
        let _env = crate::test_env::guard();
        clear_remote_submit_env_vars();
        unsafe {
            std::env::set_var(
                "AVALON_SETTLEMENT_REMOTE_URLS",
                "core=https://plural.example",
            );
            std::env::set_var("AVALON_SETTLEMENT_REMOTE_URL", "https://singular.example");
        }
        let config = RemoteSubmitConfig::from_env().expect("should be Some");
        assert_eq!(
            config.target_for_shard("core"),
            Some("https://plural.example"),
            "AVALON_SETTLEMENT_REMOTE_URLS naming core explicitly must win over the singular \
             fallback, not be silently overwritten by it"
        );
        clear_remote_submit_env_vars();
    }

    /// Issue #665: a malformed URL in `AVALON_SETTLEMENT_REMOTE_URLS` is
    /// logged and skipped, same as an entry missing `=` already was —
    /// never silently accepted as-is (which would only fail later, on the
    /// first real submit attempt) and never a hard startup failure either
    /// (this var stays optional even when malformed, unlike Indexer/
    /// Realtime — see `crate::backing_services`'s own doc comment).
    #[test]
    fn a_malformed_entry_in_the_plural_var_is_skipped_not_accepted() {
        let _env = crate::test_env::guard();
        clear_remote_submit_env_vars();
        unsafe {
            std::env::set_var(
                "AVALON_SETTLEMENT_REMOTE_URLS",
                "game:ashen-realms=not a url, core=https://core.example",
            );
        }
        let config = RemoteSubmitConfig::from_env().expect("core entry alone should be Some");
        assert_eq!(config.target_for_shard("game:ashen-realms"), None);
        assert_eq!(
            config.target_for_shard("core"),
            Some("https://core.example")
        );
        clear_remote_submit_env_vars();
    }

    /// A malformed singular `AVALON_SETTLEMENT_REMOTE_URL` with no plural
    /// var set at all falls back to no configured targets, not a panic or
    /// a silently-broken one.
    #[test]
    fn a_malformed_singular_url_with_nothing_else_configured_returns_none() {
        let _env = crate::test_env::guard();
        clear_remote_submit_env_vars();
        unsafe {
            std::env::set_var("AVALON_SETTLEMENT_REMOTE_URL", "not a url");
        }
        assert!(RemoteSubmitConfig::from_env().is_none());
        clear_remote_submit_env_vars();
    }

    #[test]
    fn remote_submit_status_starts_empty() {
        let status = RemoteSubmitStatus::default();
        assert!(status.currently_failing().is_empty());
    }

    #[test]
    fn a_recorded_failure_surfaces_the_configured_authority_url_and_reason() {
        let status = RemoteSubmitStatus::default();
        status.record_failure(
            "core",
            "https://authority.example",
            "connection refused".to_string(),
        );

        let failing = status.currently_failing();
        assert_eq!(failing.len(), 1);
        let (shard_id, failure) = &failing[0];
        assert_eq!(shard_id, "core");
        assert_eq!(failure.authority_url, "https://authority.example");
        assert_eq!(failure.reason, "connection refused");
    }

    #[test]
    fn a_success_clears_that_shards_failure_but_not_another_shards() {
        let status = RemoteSubmitStatus::default();
        status.record_failure("core", "https://a.example", "unreachable".to_string());
        status.record_failure(
            "game:ashen-realms",
            "https://b.example",
            "unauthorized".to_string(),
        );

        status.record_success("core");

        let failing = status.currently_failing();
        assert_eq!(
            failing.len(),
            1,
            "only the recovered shard's entry should clear"
        );
        assert_eq!(failing[0].0, "game:ashen-realms");
    }

    #[test]
    fn a_second_failure_for_the_same_shard_updates_the_reason_not_the_since_timestamp() {
        let status = RemoteSubmitStatus::default();
        status.record_failure(
            "core",
            "https://authority.example",
            "first reason".to_string(),
        );
        let first_since = status.currently_failing()[0].1.since;

        status.record_failure(
            "core",
            "https://authority.example",
            "second reason".to_string(),
        );
        let failing = status.currently_failing();
        assert_eq!(
            failing.len(),
            1,
            "must still be one entry for this shard, not two"
        );
        assert_eq!(failing[0].1.reason, "second reason");
        assert_eq!(
            failing[0].1.since, first_since,
            "since must track when the outage started, not this call's own timestamp"
        );
    }
}
