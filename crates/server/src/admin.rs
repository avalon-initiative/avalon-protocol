//! Hoster-only operational controls — issue #658's runtime log-level
//! endpoint, the first thing in this module. Deliberately its own module
//! (not folded into `crate::nodes`, which is public/unauthenticated reads):
//! everything here mutates or reveals process-internal state that only the
//! operator actually running this node should ever be able to touch, never
//! a peer node, an integrator, or a session.
//!
//! **Auth: a separate shared-secret bearer token (`AVALON_ADMIN_TOKEN`)**,
//! not `crate::settlement`'s `AVALON_SETTLEMENT_SUBMIT_KEY`. That key's
//! trust domain is "this operator's own nodes talking to each other" — it
//! may be shared across two nodes one hoster runs, which would let one of
//! *their own* nodes flip the other's log level. Admin control is
//! narrower: only whoever holds this one process's own credential. Same
//! posture `crate::settlement::require_settlement_submit_key` already
//! established — an unset `AVALON_ADMIN_TOKEN` means every request here is
//! refused, never silently open.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::state::AppState;

/// The concrete reload handle type `main::init_tracing` builds and
/// `AppState` carries — a filter swap through this takes effect on the
/// very next log call, no restart.
pub type LogReloadHandle =
    tracing_subscriber::reload::Handle<tracing_subscriber::EnvFilter, tracing_subscriber::Registry>;

/// Pure guard, deliberately taking just the two inputs it actually needs
/// (not the whole `AppState`) — same "factor the invariant out so it's
/// unit-testable without a live Postgres" convention `crate::recovery`'s
/// own guard functions already establish, since building a full `AppState`
/// only to exercise an auth check that never touches the database would
/// pull in live-infra weight this check has no real need for.
fn check_admin_token(configured: Option<&str>, headers: &HeaderMap) -> Result<(), AppError> {
    let expected = configured.ok_or(AppError::Unauthorized)?;
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(AppError::Unauthorized)?;
    if provided != expected {
        return Err(AppError::Unauthorized);
    }
    Ok(())
}

fn require_admin_token(state: &AppState, headers: &HeaderMap) -> Result<(), AppError> {
    check_admin_token(state.admin_token.as_deref(), headers)
}

#[derive(Serialize)]
pub struct LogLevelResponse {
    pub filter: String,
}

/// `GET /nodes/log-level` — the currently-active `RUST_LOG`-style filter
/// string, admin-token gated (see module doc comment; this reveals
/// process-internal detail, not something every caller should see).
pub async fn get_log_level(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<LogLevelResponse>, AppError> {
    require_admin_token(&state, &headers)?;
    let filter = state
        .log_reload_handle
        .with_current(|f| f.to_string())
        .map_err(|_| AppError::LogReloadFailed)?;
    Ok(Json(LogLevelResponse { filter }))
}

#[derive(Deserialize)]
pub struct SetLogLevelRequest {
    /// Any string `tracing_subscriber::EnvFilter`'s own `FromStr` accepts —
    /// the same syntax `RUST_LOG` already uses, e.g. `"debug"`,
    /// `"info,avalon_server=debug"`.
    pub filter: String,
}

/// `POST /nodes/log-level` — swaps the live filter, no restart. A filter
/// string that fails to parse is a clean `400`
/// ([`AppError::InvalidLogFilter`]) and never touches the currently-active
/// filter — a hoster typo-ing a level bump should not be able to
/// accidentally silence or flood their own logs.
pub async fn set_log_level(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<SetLogLevelRequest>,
) -> Result<Json<LogLevelResponse>, AppError> {
    require_admin_token(&state, &headers)?;
    let new_filter: tracing_subscriber::EnvFilter = body
        .filter
        .parse()
        .map_err(|_| AppError::InvalidLogFilter)?;
    let filter = new_filter.to_string();
    state
        .log_reload_handle
        .reload(new_filter)
        .map_err(|_| AppError::LogReloadFailed)?;
    tracing::info!(filter = %filter, "avalon-server: log level changed via admin endpoint");
    Ok(Json(LogLevelResponse { filter }))
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderMap;

    use super::*;

    fn headers_with_bearer(token: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    #[test]
    fn missing_admin_token_env_refuses_every_request() {
        let headers = headers_with_bearer("anything");
        assert!(matches!(
            check_admin_token(None, &headers),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn wrong_bearer_token_is_refused() {
        let headers = headers_with_bearer("wrong-token");
        assert!(matches!(
            check_admin_token(Some("real-token"), &headers),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn missing_bearer_header_is_refused() {
        assert!(matches!(
            check_admin_token(Some("real-token"), &HeaderMap::new()),
            Err(AppError::Unauthorized)
        ));
    }

    #[test]
    fn correct_bearer_token_is_accepted() {
        let headers = headers_with_bearer("real-token");
        assert!(check_admin_token(Some("real-token"), &headers).is_ok());
    }
}
