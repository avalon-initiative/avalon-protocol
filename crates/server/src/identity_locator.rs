//! Identity locator over the DHT — epic #623, issue #635. Resolves *every*
//! shard an identity has any durable history on, not just wherever
//! `identity.created` originally landed — the missing piece #634's
//! cross-node login verification needs for the general case: that module
//! only ever checks `identity_signing_keys` *locally*, so an identity whose
//! signing-key projection isn't already replicated to the node being
//! logged into can't complete verification without this.
//!
//! Reuses `crate::interest`'s existing DHT-backed registration/lookup shape
//! (`InterestScope::Identity`, issue #583/#596's mechanism, extended here)
//! rather than inventing a second DHT usage pattern. Unlike a
//! `Channel`/`Conversation` scope (registered per active websocket
//! subscriber, deregistered when the last one disconnects), an identity
//! locator entry is a standing fact about this node's own durable local
//! data — registered once discovered and never explicitly deregistered for
//! the life of the process, the same shape `crate::mirror_watcher` already
//! uses for `InterestScope::Network`.
//!
//! **Known simplification, accepted rather than solved here**: [`run_worker`]
//! rescans *every* distinct `identity_id` this node has a signing key for on
//! every tick, registering any not yet known to `registry` — correct at any
//! scale this milestone cares about, but a node with a very large number of
//! local identities would eventually want an incremental/cursor-based scan
//! instead of a full rescan. `identity_signing_keys::distinct_identity_ids`
//! is the one query this would need to change.
//!
//! **Depends on #629 (minimum replication guarantee) for the locator to be
//! meaningful**, not just mechanically correct: a locator entry pointing at
//! a shard with zero durable mirrors is a dead end regardless of how
//! correctly this module advertises and resolves it. Sequenced alongside
//! #629, not blocked on it landing first — see epic #623's own scope note.

use std::collections::HashMap;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::Json;
use serde::Serialize;
use uuid::Uuid;

use crate::interest::{InterestGuard, InterestRegistry, InterestScope};
use crate::state::AppState;

/// How often [`run_worker`] rescans local `identity_signing_keys` for
/// identities not yet registered. Independent of, and much less urgent
/// than, `interest::REFRESH_INTERVAL` (which keeps an *already-registered*
/// scope's DHT record alive) — this interval only governs how quickly a
/// brand-new local identity starts being advertised at all.
/// `AVALON_IDENTITY_LOCATOR_SCAN_INTERVAL_SECS` overrides the default,
/// same escape hatch `AVALON_ANNOUNCE_INTERVAL_SECS` already gives
/// `crate::nodes` — a live test doesn't have to wait out a real 30s
/// production interval to see a fresh registration.
const DEFAULT_SCAN_INTERVAL_SECS: u64 = 30;

fn scan_interval() -> Duration {
    std::env::var("AVALON_IDENTITY_LOCATOR_SCAN_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(DEFAULT_SCAN_INTERVAL_SECS))
}

/// Never returns. Periodically registers DHT interest
/// (`InterestScope::Identity`) for every identity this node has durable
/// `identity_signing_keys` for, holding one [`InterestGuard`] per identity
/// alive for the life of the process — `crate::interest::run_worker`
/// (already running, unmodified by this module) picks up each newly
/// registered scope and keeps re-`PutRecord`ing it, exactly as it already
/// does for `InterestScope::Network`.
pub async fn run_worker(pool: sqlx::PgPool, registry: InterestRegistry) {
    let mut guards: HashMap<Uuid, InterestGuard> = HashMap::new();
    let mut tick = tokio::time::interval(scan_interval());
    loop {
        tick.tick().await;
        let identity_ids =
            match avalon_indexer::projections::identity_signing_keys::distinct_identity_ids(&pool)
                .await
            {
                Ok(ids) => ids,
                Err(err) => {
                    tracing::error!("identity-locator: failed to scan local identities: {err}");
                    continue;
                }
            };
        for identity_id in identity_ids {
            guards
                .entry(identity_id)
                .or_insert_with(|| registry.register(InterestScope::Identity(identity_id)));
        }
    }
}

/// Resolves every location currently advertised for `identity_id` — the
/// full known set, never a single "winner," per epic #623's own scope note
/// on why a single "home" isn't enough (an integrator's own dedicated
/// settlement shard, #530, can hold real history for an identity that a
/// shared Layer-1 shard's own registration never reflects). `None` from
/// `state.dht_commands` (no DHT identity configured) resolves to an empty
/// list, same degenerate case `interest::lookup` already documents.
pub async fn resolve(state: &AppState, identity_id: Uuid) -> Vec<String> {
    let Some(dht_commands) = &state.dht_commands else {
        return Vec::new();
    };
    crate::interest::lookup(
        dht_commands,
        InterestScope::Identity(identity_id),
        state.interest_redis_fast_path.as_ref(),
    )
    .await
}

#[derive(Serialize)]
pub struct LocationsResponse {
    pub locations: Vec<String>,
}

/// `GET /identities/{id}/locations` — deliberately unauthenticated: this
/// runs *before* cross-node login can complete (a requesting node resolving
/// where to even ask), so there is routinely no session to require yet, and
/// the response (a set of server base URLs) carries no personal data —
/// same public-discovery posture `crate::nodes`'s peer-listing routes
/// already take.
pub async fn get_locations(
    State(state): State<AppState>,
    Path(identity_id): Path<Uuid>,
) -> Json<LocationsResponse> {
    Json(LocationsResponse {
        locations: resolve(&state, identity_id).await,
    })
}
