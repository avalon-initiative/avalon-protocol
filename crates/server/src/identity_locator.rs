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
//! **Registrations found within one tick are staggered ([`REGISTRATION_STAGGER`]),
//! not fired all at once.** `InterestRegistry::register`'s 0 -> 1 transition
//! always signals `interest::run_worker` to `PutRecord` immediately (issue
//! #583's own reasoning for why a fresh registration can't wait out a full
//! refresh interval) — right, for a genuinely new registration, but a node
//! with a real backlog of already-known local identities (typically right
//! after a fresh process start, before the very first scan has run at all)
//! would otherwise fire a burst of simultaneous immediate `PutRecord`s, most
//! of them landing before the DHT swarm has bootstrapped/connected to any
//! peer yet. Live-observed as a real `put_record failed: the quorum failed`
//! burst before this existed — harmless (each registration's own refresh
//! loop retries regardless), but real, avoidable noise a hoster's logs
//! don't need. Pacing registrations out fixes it directly rather than just
//! tolerating the noise.
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

/// How long to wait between registering consecutive newly-discovered
/// identities found within the same scan tick — see this module's own doc
/// comment on the startup-backlog burst this paces out. Short enough that
/// even a few hundred identities clear within seconds, long enough that
/// each immediate `PutRecord` lands as its own distinct DHT command rather
/// than all of them queuing up in the same instant.
const REGISTRATION_STAGGER: Duration = Duration::from_millis(100);

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
        register_new(identity_ids, &mut guards, &registry).await;
    }
}

/// Registers any `identity_id` in `identity_ids` not already in `guards`,
/// pausing [`REGISTRATION_STAGGER`] between each one — pulled out of
/// [`run_worker`]'s loop body so it's directly unit-testable without a real
/// Postgres pool.
async fn register_new(
    identity_ids: Vec<Uuid>,
    guards: &mut HashMap<Uuid, InterestGuard>,
    registry: &InterestRegistry,
) {
    for identity_id in identity_ids {
        if guards.contains_key(&identity_id) {
            continue;
        }
        guards.insert(
            identity_id,
            registry.register(InterestScope::Identity(identity_id)),
        );
        tokio::time::sleep(REGISTRATION_STAGGER).await;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Proves the stagger primitive itself, without a real Postgres pool or
    /// DHT swarm: registering several identities found within one (fake)
    /// scan takes at least `REGISTRATION_STAGGER` per registration, not all
    /// at once — the actual fix for the live-observed startup-backlog burst
    /// this module's own doc comment describes. `start_paused` fast-forwards
    /// simulated time rather than actually sleeping.
    #[tokio::test(start_paused = true)]
    async fn registrations_found_in_one_scan_are_staggered_not_simultaneous() {
        let (registry, _newly_active) = InterestRegistry::new();
        let ids = vec![Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let mut guards: HashMap<Uuid, InterestGuard> = HashMap::new();

        let start = tokio::time::Instant::now();
        register_new(ids.clone(), &mut guards, &registry).await;
        let elapsed = start.elapsed();

        assert_eq!(guards.len(), ids.len());
        assert!(
            elapsed >= REGISTRATION_STAGGER * (ids.len() as u32),
            "expected at least {:?} of staggered delay for {} registrations, got {:?}",
            REGISTRATION_STAGGER * (ids.len() as u32),
            ids.len(),
            elapsed
        );
    }

    /// An identity already in `guards` (already registered by an earlier
    /// scan) is skipped entirely — no re-registration, no stagger delay
    /// spent on it.
    #[tokio::test(start_paused = true)]
    async fn an_already_registered_identity_is_never_re_registered() {
        let (registry, _newly_active) = InterestRegistry::new();
        let id = Uuid::new_v4();
        let mut guards: HashMap<Uuid, InterestGuard> = HashMap::new();
        guards.insert(id, registry.register(InterestScope::Identity(id)));

        let start = tokio::time::Instant::now();
        register_new(vec![id], &mut guards, &registry).await;
        let elapsed = start.elapsed();

        assert_eq!(guards.len(), 1);
        assert!(
            elapsed < REGISTRATION_STAGGER,
            "an already-registered identity should be skipped with no stagger delay, got {elapsed:?}"
        );
    }
}
