//! Issue #665: the coherent configuration surface for a Gateway process's
//! backing-service URLs — `AVALON_INDEXER_REMOTE_URL` (#662),
//! `AVALON_REALTIME_URL` (#663), and `AVALON_SETTLEMENT_REMOTE_URL(S)`
//! (#313/#664). Each of those three tickets independently added its own
//! env var and its own small parsing function once its own role got a
//! remote mode — this module is the piece that makes the *set* of them
//! coherent, not a fourth, competing discovery mechanism (the ticket's own
//! design section is explicit: static config is enough for a first pass,
//! not a dynamic service registry).
//!
//! **What's shared, and what deliberately isn't.** All three vars name a
//! base URL an operator hand-configures, and all three should be
//! normalized and validated the same way — trimmed, trailing slash
//! stripped, and rejected if the result doesn't parse as a well-formed URL
//! (see [`normalize_and_validate_url`]). That much genuinely was
//! duplicated three times before this ticket (`nodes::realtime_mode_from_env`
//! parsed with `url::Url::parse`; `internal_role::RemoteIndexer::from_env`
//! didn't validate format at all, only non-emptiness — a real,
//! pre-existing inconsistency this ticket closes) and is now one function.
//! What's deliberately *not* unified is the "is this required, and how
//! loudly does a problem with it fail" question, because the three
//! genuinely differ there:
//! - Indexer and Realtime are each a single, all-or-nothing remote target:
//!   if the role isn't running locally, there is exactly one place to send
//!   its traffic, and a missing/malformed URL leaves this process with no
//!   way to serve that role at all — hard startup failure
//!   (`nodes::realtime_mode_from_env`, and `main.rs`'s own handling of
//!   `internal_role::RemoteIndexer::from_env`'s `Err`/`Ok(None)` cases).
//! - Settlement is a per-shard *map*, optional even when the `settlement`
//!   role is excluded (#664 established that a node can still commit
//!   locally despite its declared roles, with only a warning — see
//!   `main.rs`'s own comment on that check) — forcing it through the same
//!   single-URL, hard-fail shape would be a behavior change #664 didn't
//!   ask for and this ticket isn't here to make. `outbox::RemoteSubmitConfig::from_env`
//!   keeps its own per-entry parsing, just reusing
//!   [`normalize_and_validate_url`] for the well-formedness check instead
//!   of a fourth hand-rolled one.
//!
//! **Reachability, added by this ticket.** Before #665, every one of the
//! three checks above validated only that a configured URL *parses* — none
//! of them confirmed the backing service named by that URL was actually
//! reachable. [`check_reachable`]/[`check_all_reachable`] add that: a
//! cheap `GET {base_url}/nodes/status` (the same read-only, no-auth
//! endpoint `crate::nodes::status` already serves for every role), logged
//! as a `tracing::warn!` — never a hard failure — if it doesn't succeed.
//! **Deliberately a warning, not a `std::process::exit(1)`,** unlike the
//! missing/malformed cases above: a backing service that's briefly down
//! during a rolling restart, or simply comes up a few seconds after this
//! process does, is a normal, recoverable operational moment, not a
//! configuration error — this process still has everything it needs to
//! *try* reaching it on every real request; failing to start over a
//! transient outage would make every process in a multi-process
//! deployment fragile to the exact restart ordering used to bring the
//! deployment up. A malformed or absent URL, by contrast, can never
//! self-heal without an operator changing config, which is why those stay
//! fatal.

use std::time::Duration;

/// Bounded timeout for the reachability probe itself — generous for a
/// same-deployment call (same posture `internal_role::REMOTE_INDEXER_TIMEOUT`
/// already takes for real request traffic) while keeping a genuinely
/// unreachable target from stalling this process's startup for long.
const REACHABILITY_CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// Normalizes and validates one operator-configured backing-service base
/// URL: trims whitespace, strips a trailing slash, and rejects the result
/// if it isn't a well-formed URL. `var_name` is folded into the error
/// message purely so every call site's error reads consistently (e.g.
/// `"AVALON_REALTIME_URL is not a valid URL: ..."`) regardless of which of
/// the three vars it came from.
///
/// Does **not** decide whether an empty/unset value is an error — that's
/// each call site's own required-vs-optional question (see this module's
/// own doc comment); this function only ever validates a value the caller
/// already knows it wants to use.
pub fn normalize_and_validate_url(var_name: &str, raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/').to_string();
    if trimmed.is_empty() {
        return Err(format!("{var_name} is set but empty"));
    }
    url::Url::parse(&trimmed).map_err(|e| format!("{var_name} is not a valid URL: {e}"))?;
    Ok(trimmed)
}

/// One configured backing-service target to probe — a human-readable
/// `name` (e.g. `"indexer"`, `"realtime"`, `"settlement (shard core)"`)
/// alongside its already-validated `base_url`.
#[derive(Debug, Clone)]
pub struct BackingServiceTarget {
    pub name: String,
    pub base_url: String,
}

impl BackingServiceTarget {
    pub fn new(name: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
        }
    }
}

/// Probes one backing-service target with `GET {base_url}/nodes/status` —
/// logs a `tracing::warn!` naming the target and the failure reason if it
/// doesn't succeed, and a `tracing::debug!` on success. Never returns an
/// error to the caller: this is a diagnostic-only, best-effort check (see
/// this module's own doc comment for why it's a warning, never a startup
/// failure).
pub async fn check_reachable(client: &reqwest::Client, target: &BackingServiceTarget) {
    let url = format!("{}/nodes/status", target.base_url);
    match client
        .get(&url)
        .timeout(REACHABILITY_CHECK_TIMEOUT)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => {
            tracing::debug!(
                backing_service = %target.name,
                base_url = %target.base_url,
                "backing-service reachability check succeeded"
            );
        }
        Ok(response) => {
            tracing::warn!(
                backing_service = %target.name,
                base_url = %target.base_url,
                status = %response.status(),
                "backing-service configured but its reachability check (GET /nodes/status) \
                 returned a non-success status — this process will still start, but requests to \
                 this backing service may fail until it's reachable"
            );
        }
        Err(err) => {
            tracing::warn!(
                backing_service = %target.name,
                base_url = %target.base_url,
                error = %err,
                "backing-service configured but unreachable at startup (GET /nodes/status \
                 failed) — this process will still start (a backing service that's briefly down, \
                 e.g. mid rolling-restart, shouldn't block startup), but requests to it will fail \
                 until it's reachable"
            );
        }
    }
}

/// Runs [`check_reachable`] against every configured target, concurrently —
/// called once at startup for whichever of Indexer/Realtime/Settlement
/// backing services this process has actually configured a remote URL for
/// (a role held locally has nothing to probe). See `main.rs`'s call site.
pub async fn check_all_reachable(client: &reqwest::Client, targets: &[BackingServiceTarget]) {
    let checks = targets.iter().map(|target| check_reachable(client, target));
    futures_util::future::join_all(checks).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_well_formed_url_is_normalized_with_trailing_slash_stripped() {
        assert_eq!(
            normalize_and_validate_url("AVALON_REALTIME_URL", "http://localhost:8098/"),
            Ok("http://localhost:8098".to_string())
        );
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        assert_eq!(
            normalize_and_validate_url("AVALON_REALTIME_URL", "  http://localhost:8098  "),
            Ok("http://localhost:8098".to_string())
        );
    }

    #[test]
    fn a_malformed_url_is_rejected_with_the_var_name_in_the_message() {
        let err = normalize_and_validate_url("AVALON_INDEXER_REMOTE_URL", "not a url")
            .expect_err("must reject a malformed URL");
        assert!(err.contains("AVALON_INDEXER_REMOTE_URL"));
        assert!(err.contains("not a valid URL"));
    }

    #[test]
    fn an_empty_value_is_rejected() {
        let err =
            normalize_and_validate_url("AVALON_REALTIME_URL", "   ").expect_err("must reject");
        assert!(err.contains("AVALON_REALTIME_URL"));
        assert!(err.contains("empty"));
    }

    #[tokio::test]
    async fn check_reachable_logs_a_warning_but_never_panics_when_unreachable() {
        // Port 1 is never listenable-on in this test environment (same
        // "nothing is listening" precedent `internal_role`'s own tests use)
        // — this call must complete cleanly, not hang or panic, regardless
        // of the outcome, since it's a diagnostic-only best-effort probe.
        let client = reqwest::Client::new();
        let target = BackingServiceTarget::new("test-service", "http://127.0.0.1:1");
        check_reachable(&client, &target).await;
    }

    #[tokio::test]
    async fn check_reachable_succeeds_quietly_against_a_well_behaved_mock() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/nodes/status"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let target = BackingServiceTarget::new("test-service", server.uri());
        check_reachable(&client, &target).await;
    }

    #[tokio::test]
    async fn check_all_reachable_probes_every_target() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/nodes/status"))
            .respond_with(wiremock::ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let client = reqwest::Client::new();
        let targets = vec![
            BackingServiceTarget::new("reachable", server.uri()),
            BackingServiceTarget::new("unreachable", "http://127.0.0.1:1"),
        ];
        // Must complete without panicking regardless of per-target outcome.
        check_all_reachable(&client, &targets).await;
    }
}
