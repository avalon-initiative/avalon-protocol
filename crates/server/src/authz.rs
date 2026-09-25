//! The capability-enforcement extractor and guard — resolves
//! "who is calling, on whose behalf" (`Caller`) and "may they exercise this
//! specific capability" (`require_capability`). See
//! `docs/projects/backend-server/architecture/security-model.md`'s "Authorization: one capability,
//! one check" section for the caller-kind design,
//! why lookups key on `(identity_id, integrator_id)` not `binding_id`, and
//! why there's no cache yet. The DB-backed live proof lives in this
//! module's own `live_tests` submodule below (gated `--ignored`), not a
//! separate `crates/server/tests/*.rs` file, since every item under test
//! here is `pub(crate)` and a `tests/*.rs` file cannot see crate-private
//! items at all.
#![allow(dead_code)]

use avalon_protocol::permissions::Capability;
use axum::http::HeaderMap;
use sqlx::Row;
use uuid::Uuid;

use crate::error::AppError;
use crate::handlers::authenticate;
use crate::integrators::authenticate_integrator;
use crate::state::AppState;

/// Header naming which identity a `Caller::Integrator` request acts on behalf of
/// — see this module's doc comment for why this is an identity, not a
/// binding, id.
const CALLER_IDENTITY_ID_HEADER: &str = "x-avalon-identity-id";

/// Who is calling, resolved by [`authenticate_caller`]. A handler matches
/// on this to decide what it's allowed to assume about the request, and
/// (for [`Caller::Integrator`]) calls [`require_capability`] naming the exact
/// capability it needs before touching anything user-owned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Caller {
    /// A user's own authenticated session — full access to their own
    /// resources, no grant needed (see module doc comment).
    User(Uuid),
    /// An integrator, authenticated via challenge-response
    /// (`integrators::authenticate_integrator`), acting on behalf of `identity_id`.
    /// Nothing about this variant alone implies access to anything —
    /// [`require_capability`] is what actually authorizes a specific
    /// action.
    Integrator {
        integrator_id: Uuid,
        identity_id: Uuid,
    },
}

/// Resolves [`Caller`] from a request: a user bearer session
/// (`Authorization: Bearer <token>`) if present, otherwise the integrator
/// challenge-response credential plus the identity header, otherwise
/// `AppError::Unauthorized`. User session takes priority — a request
/// carrying both a user bearer token and integrator challenge-response headers
/// (not a real scenario today, but not forbidden by the header shapes
/// alone) is treated as the user, since that's the stronger, more
/// specific proof and there's no ambiguity to resolve either way.
pub(crate) async fn authenticate_caller(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Caller, AppError> {
    if headers.contains_key(axum::http::header::AUTHORIZATION) {
        let identity_id = authenticate(state, headers).await?;
        return Ok(Caller::User(identity_id));
    }

    let integrator_id = authenticate_integrator(state, headers).await?;
    let identity_id: Uuid = headers
        .get(CALLER_IDENTITY_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or(AppError::Unauthorized)?;
    Ok(Caller::Integrator {
        integrator_id,
        identity_id,
    })
}

/// Everything [`capability_authorized`] needs to know about an identity's
/// binding to a specific integrator, decoupled from how it was fetched — the
/// same decomposition `guilds.rs`'s `has_guild_permission` uses to keep
/// authorization logic testable without a database. `bound_integrator_id` is the
/// integrator the binding actually belongs to; `capability_authorized` compares
/// it against the *caller's* integrator id rather than trusting the caller.
struct BindingFacts {
    bound_integrator_id: Uuid,
    active: bool,
}

/// Everything [`capability_authorized`] needs to know about a specific
/// `(binding, capability)` grant.
struct GrantFacts {
    active: bool,
}

/// The entire authorization decision for `Caller::Integrator`, pure and
/// DB-free: true only if a binding exists, it belongs to exactly
/// `caller_integrator_id`, it is active (not ended), and a grant for the
/// requested capability exists and is active (not revoked). Every other
/// combination — no binding, an ended binding, a binding for a different
/// integrator, no grant at all, a revoked grant — is false. This is the one and
/// only check; there is no separate `integrator_has_access_to_user` boolean
/// anywhere in this module.
fn capability_authorized(
    caller_integrator_id: Uuid,
    binding: Option<&BindingFacts>,
    grant: Option<&GrantFacts>,
) -> bool {
    let Some(binding) = binding else {
        return false;
    };
    if binding.bound_integrator_id != caller_integrator_id || !binding.active {
        return false;
    }
    grant.is_some_and(|g| g.active)
}

/// The active `bindings` row (if any) for `(identity_id, integrator_id)`, plus
/// its id — same query shape `connections.rs`'s `active_binding` uses
/// (scoped by both columns, `ended_at IS NULL`), just against the pool
/// directly rather than an in-flight transaction, since this is a
/// read-only check with nothing to lock against.
async fn fetch_binding(
    state: &AppState,
    identity_id: Uuid,
    integrator_id: Uuid,
) -> Result<Option<(Uuid, BindingFacts)>, AppError> {
    let row = sqlx::query(
        "SELECT id, integrator_id FROM bindings WHERE identity_id = $1 AND integrator_id = $2 \
         AND ended_at IS NULL",
    )
    .bind(identity_id)
    .bind(integrator_id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(match row {
        Some(row) => {
            let binding_id: Uuid = row.try_get("id")?;
            let bound_integrator_id: Uuid = row.try_get("integrator_id")?;
            Some((
                binding_id,
                BindingFacts {
                    bound_integrator_id,
                    active: true, // the WHERE clause already guarantees this
                },
            ))
        }
        None => None,
    })
}

/// The active `permission_grants` row (if any) for `(binding_id,
/// capability)` — same query shape `connections.rs` already uses
/// (`revoked_at IS NULL`).
async fn fetch_grant(
    state: &AppState,
    binding_id: Uuid,
    capability: &str,
) -> Result<Option<GrantFacts>, AppError> {
    let row = sqlx::query(
        "SELECT 1 FROM permission_grants WHERE binding_id = $1 AND capability = $2 \
         AND revoked_at IS NULL",
    )
    .bind(binding_id)
    .bind(capability)
    .fetch_optional(&state.pool)
    .await?;
    Ok(row.map(|_| GrantFacts { active: true }))
}

/// Whether an active `bindings` row exists for `(identity_id, integrator_id)`,
/// with no specific capability grant required — the user-consent check
/// `integrator_data::publish_instance` uses, matching
/// `issue_attestation`'s "an active binding to this issuer" language but
/// without a capability grant on top (publishing
/// instance data about a bound user is closer to schema/achievement
/// *definition* than to acting on a user's other resources). Reuses
/// [`fetch_binding`] rather than a second hand-rolled query, same posture
/// this module's own doc comment insists on for every other binding check.
pub(crate) async fn has_active_binding(
    state: &AppState,
    identity_id: Uuid,
    integrator_id: Uuid,
) -> Result<bool, AppError> {
    Ok(fetch_binding(state, identity_id, integrator_id)
        .await?
        .is_some_and(|(_, facts)| facts.bound_integrator_id == integrator_id && facts.active))
}

/// The guard: does `caller` have `capability`? `capability` is mandatory
/// (not `Option`, no default) so a call site can never accidentally check
/// nothing — see module doc comment. `Caller::User` always passes
/// (a user always has full access to their own data); `Caller::Integrator`
/// resolves its active binding and the specific grant for `capability`
/// fresh from `permission_grants` (see module doc comment on why there's
/// no cache yet) and returns `AppError::Forbidden` unless both are active
/// right now. The 403 body never says which of "no binding" / "binding
/// for the wrong integrator" / "no grant" / "grant revoked" applies — same
/// "never leak internal detail" posture `AppError::into_response`
/// documents for every other variant.
pub(crate) async fn require_capability(
    caller: &Caller,
    capability: Capability,
    state: &AppState,
) -> Result<(), AppError> {
    match caller {
        Caller::User(_) => Ok(()),
        Caller::Integrator {
            integrator_id,
            identity_id,
        } => {
            let binding = fetch_binding(state, *identity_id, *integrator_id).await?;
            let grant = match &binding {
                Some((binding_id, facts)) if facts.active => {
                    fetch_grant(state, *binding_id, capability.as_str()).await?
                }
                _ => None,
            };
            let binding_facts = binding.as_ref().map(|(_, facts)| facts);
            if capability_authorized(*integrator_id, binding_facts, grant.as_ref()) {
                Ok(())
            } else {
                Err(AppError::Forbidden)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — pure-logic checks only, exercising
    //! `capability_authorized` directly. The DB-backed end-to-end version
    //! of this same matrix (seed a binding/grant, call `require_capability`
    //! for real, revoke via #27's endpoint, call it again) is
    //! this module's own `live_tests` submodule below, gated `--ignored`.

    use super::*;

    fn active_binding(integrator_id: Uuid) -> BindingFacts {
        BindingFacts {
            bound_integrator_id: integrator_id,
            active: true,
        }
    }

    fn ended_binding(integrator_id: Uuid) -> BindingFacts {
        BindingFacts {
            bound_integrator_id: integrator_id,
            active: false,
        }
    }

    fn active_grant() -> GrantFacts {
        GrantFacts { active: true }
    }

    #[test]
    fn active_binding_and_active_grant_is_authorized() {
        let integrator_id = Uuid::new_v4();
        assert!(capability_authorized(
            integrator_id,
            Some(&active_binding(integrator_id)),
            Some(&active_grant()),
        ));
    }

    #[test]
    fn no_binding_at_all_is_rejected() {
        let integrator_id = Uuid::new_v4();
        assert!(!capability_authorized(
            integrator_id,
            None,
            Some(&active_grant())
        ));
    }

    #[test]
    fn an_ended_binding_is_rejected_even_with_an_active_grant() {
        let integrator_id = Uuid::new_v4();
        assert!(!capability_authorized(
            integrator_id,
            Some(&ended_binding(integrator_id)),
            Some(&active_grant()),
        ));
    }

    #[test]
    fn a_binding_for_a_different_integrator_is_rejected() {
        // The binding is real and active, but it belongs to Integrator A while
        // the caller authenticated as Integrator B — using one user's
        // binding-to-Integrator-A to claim access via Integrator B must never work.
        let integrator_a = Uuid::new_v4();
        let integrator_b = Uuid::new_v4();
        assert!(!capability_authorized(
            integrator_b,
            Some(&active_binding(integrator_a)),
            Some(&active_grant()),
        ));
    }

    #[test]
    fn an_active_binding_with_no_grant_at_all_is_rejected() {
        let integrator_id = Uuid::new_v4();
        assert!(!capability_authorized(
            integrator_id,
            Some(&active_binding(integrator_id)),
            None,
        ));
    }

    #[test]
    fn a_revoked_grant_is_rejected() {
        let integrator_id = Uuid::new_v4();
        assert!(!capability_authorized(
            integrator_id,
            Some(&active_binding(integrator_id)),
            Some(&GrantFacts { active: false }),
        ));
    }

    #[test]
    fn no_binding_and_no_grant_together_is_rejected() {
        let integrator_id = Uuid::new_v4();
        assert!(!capability_authorized(integrator_id, None, None));
    }

    #[test]
    fn user_caller_is_a_distinct_variant_never_fed_into_capability_authorized() {
        // `require_capability`'s `Caller::User` arm short-circuits to
        // `Ok(())` before `capability_authorized` (a `Caller::Integrator`-only
        // helper) is ever called — this documents that shape rather than
        // calling the helper with meaningless inputs.
        let caller = Caller::User(Uuid::new_v4());
        assert!(matches!(caller, Caller::User(_)));
    }
}

#[cfg(test)]
mod live_tests {
    //! Exercises `require_capability` against a real, seeded binding/grant
    //! in Postgres — gated `--ignored` since it needs live infra, same as
    //! every `crates/server/tests/*.rs` file, but living here instead of
    //! there (see this module's own top-level doc comment for why: a
    //! `tests/*.rs` file is a separate crate and cannot see `pub(crate)`
    //! items at all). Skipped in this sandbox (no
    //! reachable Postgres here) — written but not run against a live
    //! database.
    //!
    //! Revokes through `connections::revoke_grant` directly — the exact
    //! handler function `DELETE /integrations/{slug}/grants/{capability}` runs,
    //! called in-process rather than over HTTP — so this proves the guard
    //! reads state a real revoke produced, not a hand-rolled SQL shortcut
    //! that happens to look the same.

    use axum::extract::{Path, State};
    use sqlx::postgres::PgPoolOptions;
    use sqlx::PgPool;
    use time::OffsetDateTime;

    use super::*;
    use crate::connections::revoke_grant;
    use crate::presence::PresenceStore;

    async fn test_pool() -> PgPool {
        let database_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
        PgPoolOptions::new()
            .connect(&database_url)
            .await
            .expect("failed to connect to Postgres — is it reachable?")
    }

    /// A throwaway `AppState` — same construction `main.rs` does, minus
    /// the outbox worker and real `AVALON_WEBAUTHN_*` env vars (nothing in
    /// this test performs a WebAuthn ceremony, so the RP id/origin values
    /// are never actually exercised).
    async fn test_state(pool: PgPool) -> AppState {
        let webauthn = std::sync::Arc::new(
            crate::auth::build_webauthn("localhost", "http://localhost:8080")
                .expect("failed to build a throwaway Webauthn instance for this test"),
        );
        let chain = avalon_chain::PostgresSettlementProvider::new(pool.clone(), "avalon-test");
        let indexer = avalon_indexer::postgres::PostgresIndexer::new(pool.clone());
        AppState {
            pool,
            chain,
            indexer: crate::state::IndexerHandle::Local(indexer),
            webauthn,
            presence: PresenceStore::from_env(),
            chat: crate::chat::ChatBus::new(),
            settlement_submit_key: None,
            peers: crate::nodes::PeerTable::new(),
            managed_hosting_verify_key: None,
            known_shards: None,
            remote_submit_status: None,
            own_shard_id: "core".to_string(),
            shard_mirror_sources: crate::settlement::ShardMirrorSources::default(),
            interest: crate::interest::InterestRegistry::new().0,
            dht_commands: None,
            own_base_url: None,
            own_libp2p_peer_id: None,
            interest_redis_fast_path: None,
            principal_limiter: crate::principal_limits::PrincipalLimiter::from_env(None),
            mirror_wake: std::sync::Arc::new(tokio::sync::Notify::new()),
            host_metrics: crate::resources::HostMetricsSampler::new(Vec::new()),
            shard_registry: crate::nodes::ShardRegistry::new(),
            head_gossip: crate::nodes::HeadGossipTracker::new(),
            admin_token: None,
            log_reload_handle: tracing_subscriber::reload::Layer::new(
                tracing_subscriber::EnvFilter::new("info"),
            )
            .1,
            internal_role_key: None,
            mirror_confirmations: crate::replication::MirrorConfirmationRegistry::new(),
            replication_gate: crate::replication::ReplicationGateConfig::from_env(),
            realtime_remote_url: None,
            known_list: crate::known_list::KnownListHandle::load_or_new(
                crate::known_list::KnownListConfig::default(),
                None,
            ),
            own_witness: None,
        }
    }

    struct Seeded {
        identity_id: Uuid,
        token: String,
        integrator_id: Uuid,
        slug: String,
    }

    /// Seeds a bare identity + session, a registered integrator declaring
    /// `friends.read`, and an active binding + grant between them —
    /// direct SQL, same rows `POST /integrations/{slug}/connect` would produce,
    /// since this test isn't going over HTTP at all (see module doc
    /// comment).
    async fn seed(pool: &PgPool) -> Seeded {
        let identity_id = Uuid::new_v4();
        sqlx::query("INSERT INTO identities (id) VALUES ($1)")
            .bind(identity_id)
            .execute(pool)
            .await
            .expect("failed to seed identity");
        sqlx::query("INSERT INTO profiles (identity_id, display_name) VALUES ($1, $2)")
            .bind(identity_id)
            .bind(format!("authz-test-{identity_id}"))
            .execute(pool)
            .await
            .expect("failed to seed profile");

        let token = format!("test-token-{}", Uuid::new_v4());
        sqlx::query("INSERT INTO sessions (token, identity_id, expires_at) VALUES ($1, $2, $3)")
            .bind(&token)
            .bind(identity_id)
            .bind(OffsetDateTime::now_utc() + time::Duration::hours(1))
            .execute(pool)
            .await
            .expect("failed to seed session");

        let integrator_id = Uuid::new_v4();
        let slug = format!("authz-test-{}", Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO integrators (id, slug, name, owner_name, registered_at, status) \
             VALUES ($1, $2, $3, $4, $5, 'active')",
        )
        .bind(integrator_id)
        .bind(&slug)
        .bind("Authz Test Integrator")
        .bind("Test Studio")
        .bind(OffsetDateTime::now_utc())
        .execute(pool)
        .await
        .expect("failed to seed integrator");
        sqlx::query(
            "INSERT INTO integrator_requested_capabilities (integrator_id, capability) VALUES ($1, $2)",
        )
        .bind(integrator_id)
        .bind("friends.read")
        .execute(pool)
        .await
        .expect("failed to seed requested capability");

        let binding_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO bindings (id, identity_id, integrator_id, established_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(binding_id)
        .bind(identity_id)
        .bind(integrator_id)
        .bind(OffsetDateTime::now_utc())
        .execute(pool)
        .await
        .expect("failed to seed binding");
        sqlx::query(
            "INSERT INTO permission_grants (id, binding_id, capability, granted_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(Uuid::new_v4())
        .bind(binding_id)
        .bind("friends.read")
        .bind(OffsetDateTime::now_utc())
        .execute(pool)
        .await
        .expect("failed to seed grant");

        Seeded {
            identity_id,
            token,
            integrator_id,
            slug,
        }
    }

    #[tokio::test]
    #[ignore]
    async fn an_active_grant_passes_then_a_real_revoke_makes_the_next_check_fail() {
        let pool = test_pool().await;
        let seeded = seed(&pool).await;
        let state = test_state(pool).await;

        let caller = Caller::Integrator {
            integrator_id: seeded.integrator_id,
            identity_id: seeded.identity_id,
        };

        require_capability(&caller, Capability::FriendsRead, &state)
            .await
            .expect("an active binding + active grant must be authorized");

        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {}", seeded.token).parse().unwrap(),
        );
        let _ = revoke_grant(
            State(state.clone()),
            headers,
            Path((seeded.slug.clone(), "friends.read".to_string())),
        )
        .await
        .expect("revoke should succeed");

        let after = require_capability(&caller, Capability::FriendsRead, &state).await;
        assert!(
            matches!(after, Err(AppError::Forbidden)),
            "a revoked grant must be rejected on the very next check, no grace window: {after:?}"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn no_grant_at_all_is_rejected_against_a_real_seeded_binding() {
        let pool = test_pool().await;
        let seeded = seed(&pool).await;
        let state = test_state(pool).await;

        // `friends.read` was seeded; `presence.read` was never granted at
        // all under this same active binding.
        let caller = Caller::Integrator {
            integrator_id: seeded.integrator_id,
            identity_id: seeded.identity_id,
        };
        let result = require_capability(&caller, Capability::PresenceRead, &state).await;
        assert!(matches!(result, Err(AppError::Forbidden)));
    }

    #[tokio::test]
    #[ignore]
    async fn a_binding_to_a_different_integrator_is_rejected_against_real_seeded_rows() {
        let pool = test_pool().await;
        let seeded = seed(&pool).await;
        let state = test_state(pool).await;

        // A real active binding + grant exist for `seeded.integrator_id`, but
        // the caller claims to be a different integrator entirely — no
        // `bindings` row exists for `(identity_id, other_integrator_id)`, so
        // this must fail exactly like "no binding at all".
        let other_integrator_id = Uuid::new_v4();
        let caller = Caller::Integrator {
            integrator_id: other_integrator_id,
            identity_id: seeded.identity_id,
        };
        let result = require_capability(&caller, Capability::FriendsRead, &state).await;
        assert!(matches!(result, Err(AppError::Forbidden)));
    }

    #[tokio::test]
    #[ignore]
    async fn user_caller_always_passes_regardless_of_any_grant_state() {
        let pool = test_pool().await;
        let seeded = seed(&pool).await;
        let state = test_state(pool).await;

        // The user themself, not an integrator — passes trivially even though
        // no integrator-side binding/grant check is relevant at all.
        let caller = Caller::User(seeded.identity_id);
        require_capability(&caller, Capability::WalletWrite, &state)
            .await
            .expect("a user always has full access to their own data");
    }
}
