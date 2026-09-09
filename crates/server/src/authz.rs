//! The capability-enforcement extractor and guard (issue #28) — resolves
//! "who is calling, on whose behalf" (`Caller`) and "may they exercise this
//! specific capability" (`require_capability`), so a future endpoint that
//! lets a game act on a player's behalf never has to hand-roll either
//! question.
//!
//! **No caller exists for this yet.** As of this ticket, every real
//! endpoint in this repo is either player-session-only (`friends.rs`,
//! `guilds.rs`, `connections.rs` — a grant is a player action, a game never
//! grants itself anything) or game-credential-only with nothing
//! player-specific to check (`games::game_whoami`, which only proves the
//! game's own identity). `Caller`/`require_capability` are infrastructure
//! for the first future ticket that adds a real game-calling-the-API
//! endpoint (achievement issuance, etc.) — see this module's own test
//! matrix below and this module's own `live_tests` submodule for the coverage that
//! stands in for a downstream handler that doesn't exist yet. **Any future
//! endpoint that lets a game act on a player's behalf must call
//! [`require_capability`] rather than inventing its own check** — a
//! hand-rolled `game_has_access_to_player` boolean is exactly the failure
//! mode issue #28 exists to close off.
//!
//! The DB-backed proof that this guard reads live state correctly (seed a
//! binding/grant, pass, revoke through issue #27's real handler function,
//! fail) lives in this module's own `live_tests` submodule below, gated
//! `--ignored` — **not** a separate file under `crates/server/tests/`, the
//! pattern every other integration test in this repo follows. Every item
//! under test here (`Caller`, `require_capability`) is `pub(crate)`, and a
//! `tests/*.rs` file compiles as its own separate crate that links against
//! `avalon-server` as a library — it cannot see crate-private items at
//! all, so it could not call `require_capability` even in principle. Living
//! inside the crate is what makes calling it possible; everything else
//! about the test (real Postgres, `--ignored`, not run in this sandbox) is
//! unchanged from the established pattern.
//!
//! ## Two caller kinds, one extractor
//!
//! [`Caller::Player`] is the existing bearer-session flow
//! (`crate::handlers::authenticate`) — unchanged, just wrapped. A player
//! always has full access to their own resources; [`require_capability`]
//! passes trivially for this variant, since this guard is specifically
//! about *game* access to *player* data, not about a player's access to
//! themselves.
//!
//! [`Caller::Game`] is `games::authenticate_game`'s existing
//! challenge-response game-credential proof, plus one more thing a game
//! credential alone can never supply: *which player* the game is acting
//! for. A game's signature only proves the game's own identity — it says
//! nothing about which identity granted it anything. This extractor reads
//! that from a new `x-avalon-identity-id` header, sent alongside the
//! existing `x-avalon-game-key-id` / `x-avalon-game-challenge-id` /
//! `x-avalon-game-signature` headers `authenticate_game` already reads.
//!
//! **Why `identity_id`, not `binding_id`.** `Caller::Game`'s fields are
//! `{ game_id, identity_id }` — an identity header keeps that struct
//! self-describing and keeps [`require_capability`] doing the one real
//! lookup that matters: "is there an active binding **for this
//! (identity_id, game_id) pair specifically**, and an active grant for
//! this exact capability under it." A `binding_id` header would let a
//! caller name a row without the guard needing to confirm *whose* row it
//! is or *which game* it belongs to — reintroducing exactly the kind of
//! implicit trust ("this id must be legitimate, since it parses") this
//! ticket exists to remove. Resolving by `(identity_id, game_id)` instead
//! means the lookup itself enforces "a game can't use one player's
//! binding-to-Game-A to claim access via Game B" — there is no `bindings`
//! row to find under a mismatched game, full stop, rather than a row being
//! found and then rejected after the fact.
//!
//! ## No cache (yet)
//!
//! The ticket suggests a short in-process cache keyed by
//! `(identity, game, capability)`. Not built here: with no real caller of
//! `require_capability` yet, there is nothing to profile a cache against,
//! and a wrong invalidation rule (the one hard part of any cache) would be
//! actively dangerous for an authorization check — "revoked is rejected on
//! the next request, no grace window" is the invariant, and a stale cache
//! entry is precisely the shape of bug that would silently violate it. A
//! plain DB read per check is correct today; add the cache once a real
//! caller exists to measure the read volume against.
//!
//! `#![allow(dead_code)]`: every `pub(crate)` item here is real,
//! documented, and covered by this module's own tests plus
//! this module's own `live_tests` submodule — it's just genuinely unused by any
//! handler yet, per this module's own doc comment above. Suppressing here
//! rather than per-item keeps that one fact in one place instead of
//! scattered across every `fn`.
#![allow(dead_code)]

use avalon_protocol::permissions::Capability;
use axum::http::HeaderMap;
use sqlx::Row;
use uuid::Uuid;

use crate::error::AppError;
use crate::games::authenticate_game;
use crate::handlers::authenticate;
use crate::state::AppState;

/// Header naming which identity a `Caller::Game` request acts on behalf of
/// — see this module's doc comment for why this is an identity, not a
/// binding, id.
const CALLER_IDENTITY_ID_HEADER: &str = "x-avalon-identity-id";

/// Who is calling, resolved by [`authenticate_caller`]. A handler matches
/// on this to decide what it's allowed to assume about the request, and
/// (for [`Caller::Game`]) calls [`require_capability`] naming the exact
/// capability it needs before touching anything player-owned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Caller {
    /// A player's own authenticated session — full access to their own
    /// resources, no grant needed (see module doc comment).
    Player(Uuid),
    /// A game, authenticated via challenge-response
    /// (`games::authenticate_game`), acting on behalf of `identity_id`.
    /// Nothing about this variant alone implies access to anything —
    /// [`require_capability`] is what actually authorizes a specific
    /// action.
    Game { game_id: Uuid, identity_id: Uuid },
}

/// Resolves [`Caller`] from a request: a player bearer session
/// (`Authorization: Bearer <token>`) if present, otherwise the game
/// challenge-response credential plus the identity header, otherwise
/// `AppError::Unauthorized`. Player session takes priority — a request
/// carrying both a player bearer token and game challenge-response headers
/// (not a real scenario today, but not forbidden by the header shapes
/// alone) is treated as the player, since that's the stronger, more
/// specific proof and there's no ambiguity to resolve either way.
pub(crate) async fn authenticate_caller(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Caller, AppError> {
    if headers.contains_key(axum::http::header::AUTHORIZATION) {
        let identity_id = authenticate(state, headers).await?;
        return Ok(Caller::Player(identity_id));
    }

    let game_id = authenticate_game(state, headers).await?;
    let identity_id: Uuid = headers
        .get(CALLER_IDENTITY_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or(AppError::Unauthorized)?;
    Ok(Caller::Game {
        game_id,
        identity_id,
    })
}

/// Everything [`capability_authorized`] needs to know about an identity's
/// binding to a specific game, decoupled from how it was fetched — the
/// same decomposition `guilds.rs`'s `has_guild_permission` uses to keep
/// authorization logic testable without a database. `bound_game_id` is the
/// game the binding actually belongs to; `capability_authorized` compares
/// it against the *caller's* game id rather than trusting the caller.
struct BindingFacts {
    bound_game_id: Uuid,
    active: bool,
}

/// Everything [`capability_authorized`] needs to know about a specific
/// `(binding, capability)` grant.
struct GrantFacts {
    active: bool,
}

/// The entire authorization decision for `Caller::Game`, pure and
/// DB-free: true only if a binding exists, it belongs to exactly
/// `caller_game_id`, it is active (not ended), and a grant for the
/// requested capability exists and is active (not revoked). Every other
/// combination — no binding, an ended binding, a binding for a different
/// game, no grant at all, a revoked grant — is false. This is the one and
/// only check; there is no separate `game_has_access_to_player` boolean
/// anywhere in this module.
fn capability_authorized(
    caller_game_id: Uuid,
    binding: Option<&BindingFacts>,
    grant: Option<&GrantFacts>,
) -> bool {
    let Some(binding) = binding else {
        return false;
    };
    if binding.bound_game_id != caller_game_id || !binding.active {
        return false;
    }
    grant.is_some_and(|g| g.active)
}

/// The active `bindings` row (if any) for `(identity_id, game_id)`, plus
/// its id — same query shape `connections.rs`'s `active_binding` uses
/// (scoped by both columns, `ended_at IS NULL`), just against the pool
/// directly rather than an in-flight transaction, since this is a
/// read-only check with nothing to lock against.
async fn fetch_binding(
    state: &AppState,
    identity_id: Uuid,
    game_id: Uuid,
) -> Result<Option<(Uuid, BindingFacts)>, AppError> {
    let row = sqlx::query(
        "SELECT id, game_id FROM bindings WHERE identity_id = $1 AND game_id = $2 \
         AND ended_at IS NULL",
    )
    .bind(identity_id)
    .bind(game_id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(match row {
        Some(row) => {
            let binding_id: Uuid = row.try_get("id")?;
            let bound_game_id: Uuid = row.try_get("game_id")?;
            Some((
                binding_id,
                BindingFacts {
                    bound_game_id,
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

/// The guard: does `caller` have `capability`? `capability` is mandatory
/// (not `Option`, no default) so a call site can never accidentally check
/// nothing — see module doc comment. `Caller::Player` always passes
/// (a player always has full access to their own data); `Caller::Game`
/// resolves its active binding and the specific grant for `capability`
/// fresh from `permission_grants` (see module doc comment on why there's
/// no cache yet) and returns `AppError::Forbidden` unless both are active
/// right now. The 403 body never says which of "no binding" / "binding
/// for the wrong game" / "no grant" / "grant revoked" applies — same
/// "never leak internal detail" posture `AppError::into_response`
/// documents for every other variant.
pub(crate) async fn require_capability(
    caller: &Caller,
    capability: Capability,
    state: &AppState,
) -> Result<(), AppError> {
    match caller {
        Caller::Player(_) => Ok(()),
        Caller::Game {
            game_id,
            identity_id,
        } => {
            let binding = fetch_binding(state, *identity_id, *game_id).await?;
            let grant = match &binding {
                Some((binding_id, facts)) if facts.active => {
                    fetch_grant(state, *binding_id, capability.as_str()).await?
                }
                _ => None,
            };
            let binding_facts = binding.as_ref().map(|(_, facts)| facts);
            if capability_authorized(*game_id, binding_facts, grant.as_ref()) {
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

    fn active_binding(game_id: Uuid) -> BindingFacts {
        BindingFacts {
            bound_game_id: game_id,
            active: true,
        }
    }

    fn ended_binding(game_id: Uuid) -> BindingFacts {
        BindingFacts {
            bound_game_id: game_id,
            active: false,
        }
    }

    fn active_grant() -> GrantFacts {
        GrantFacts { active: true }
    }

    #[test]
    fn active_binding_and_active_grant_is_authorized() {
        let game_id = Uuid::new_v4();
        assert!(capability_authorized(
            game_id,
            Some(&active_binding(game_id)),
            Some(&active_grant()),
        ));
    }

    #[test]
    fn no_binding_at_all_is_rejected() {
        let game_id = Uuid::new_v4();
        assert!(!capability_authorized(game_id, None, Some(&active_grant())));
    }

    #[test]
    fn an_ended_binding_is_rejected_even_with_an_active_grant() {
        let game_id = Uuid::new_v4();
        assert!(!capability_authorized(
            game_id,
            Some(&ended_binding(game_id)),
            Some(&active_grant()),
        ));
    }

    #[test]
    fn a_binding_for_a_different_game_is_rejected() {
        // The binding is real and active, but it belongs to Game A while
        // the caller authenticated as Game B — using one player's
        // binding-to-Game-A to claim access via Game B must never work.
        let game_a = Uuid::new_v4();
        let game_b = Uuid::new_v4();
        assert!(!capability_authorized(
            game_b,
            Some(&active_binding(game_a)),
            Some(&active_grant()),
        ));
    }

    #[test]
    fn an_active_binding_with_no_grant_at_all_is_rejected() {
        let game_id = Uuid::new_v4();
        assert!(!capability_authorized(
            game_id,
            Some(&active_binding(game_id)),
            None,
        ));
    }

    #[test]
    fn a_revoked_grant_is_rejected() {
        let game_id = Uuid::new_v4();
        assert!(!capability_authorized(
            game_id,
            Some(&active_binding(game_id)),
            Some(&GrantFacts { active: false }),
        ));
    }

    #[test]
    fn no_binding_and_no_grant_together_is_rejected() {
        let game_id = Uuid::new_v4();
        assert!(!capability_authorized(game_id, None, None));
    }

    #[test]
    fn player_caller_is_a_distinct_variant_never_fed_into_capability_authorized() {
        // `require_capability`'s `Caller::Player` arm short-circuits to
        // `Ok(())` before `capability_authorized` (a `Caller::Game`-only
        // helper) is ever called — this documents that shape rather than
        // calling the helper with meaningless inputs.
        let caller = Caller::Player(Uuid::new_v4());
        assert!(matches!(caller, Caller::Player(_)));
    }
}

#[cfg(test)]
mod live_tests {
    //! Exercises `require_capability` against a real, seeded binding/grant
    //! in Postgres — gated `--ignored` since it needs live infra, same as
    //! every `crates/server/tests/*.rs` file, but living here instead of
    //! there (see this module's own top-level doc comment for why: a
    //! `tests/*.rs` file is a separate crate and cannot see `pub(crate)`
    //! items at all). Skipped in this sandbox per `.claude/CLAUDE.md` (no
    //! reachable Postgres here) — written but not run against a live
    //! database.
    //!
    //! Revokes through `connections::revoke_grant` directly — the exact
    //! handler function `DELETE /games/{slug}/grants/{capability}` runs,
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
        let chain = avalon_chain::PostgresSettlementProvider::new(pool.clone());
        let indexer = avalon_indexer::postgres::PostgresIndexer::new(pool.clone());
        AppState {
            pool,
            chain,
            indexer,
            webauthn,
            presence: PresenceStore::from_env(),
        }
    }

    struct Seeded {
        identity_id: Uuid,
        token: String,
        game_id: Uuid,
        slug: String,
    }

    /// Seeds a bare identity + session, a registered game declaring
    /// `friends.read`, and an active binding + grant between them —
    /// direct SQL, same rows `POST /games/{slug}/connect` would produce,
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

        let game_id = Uuid::new_v4();
        let slug = format!("authz-test-{}", Uuid::new_v4().simple());
        sqlx::query(
            "INSERT INTO games (id, slug, name, developer, registered_at, status) \
             VALUES ($1, $2, $3, $4, $5, 'active')",
        )
        .bind(game_id)
        .bind(&slug)
        .bind("Authz Test Game")
        .bind("Test Studio")
        .bind(OffsetDateTime::now_utc())
        .execute(pool)
        .await
        .expect("failed to seed game");
        sqlx::query(
            "INSERT INTO game_requested_capabilities (game_id, capability) VALUES ($1, $2)",
        )
        .bind(game_id)
        .bind("friends.read")
        .execute(pool)
        .await
        .expect("failed to seed requested capability");

        let binding_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO bindings (id, identity_id, game_id, established_at) \
             VALUES ($1, $2, $3, $4)",
        )
        .bind(binding_id)
        .bind(identity_id)
        .bind(game_id)
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
            game_id,
            slug,
        }
    }

    #[tokio::test]
    #[ignore]
    async fn an_active_grant_passes_then_a_real_revoke_makes_the_next_check_fail() {
        let pool = test_pool().await;
        let seeded = seed(&pool).await;
        let state = test_state(pool).await;

        let caller = Caller::Game {
            game_id: seeded.game_id,
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
        let caller = Caller::Game {
            game_id: seeded.game_id,
            identity_id: seeded.identity_id,
        };
        let result = require_capability(&caller, Capability::PresenceRead, &state).await;
        assert!(matches!(result, Err(AppError::Forbidden)));
    }

    #[tokio::test]
    #[ignore]
    async fn a_binding_to_a_different_game_is_rejected_against_real_seeded_rows() {
        let pool = test_pool().await;
        let seeded = seed(&pool).await;
        let state = test_state(pool).await;

        // A real active binding + grant exist for `seeded.game_id`, but
        // the caller claims to be a different game entirely — no
        // `bindings` row exists for `(identity_id, other_game_id)`, so
        // this must fail exactly like "no binding at all".
        let other_game_id = Uuid::new_v4();
        let caller = Caller::Game {
            game_id: other_game_id,
            identity_id: seeded.identity_id,
        };
        let result = require_capability(&caller, Capability::FriendsRead, &state).await;
        assert!(matches!(result, Err(AppError::Forbidden)));
    }

    #[tokio::test]
    #[ignore]
    async fn player_caller_always_passes_regardless_of_any_grant_state() {
        let pool = test_pool().await;
        let seeded = seed(&pool).await;
        let state = test_state(pool).await;

        // The player themself, not a game — passes trivially even though
        // no game-side binding/grant check is relevant at all.
        let caller = Caller::Player(seeded.identity_id);
        require_capability(&caller, Capability::WalletWrite, &state)
            .await
            .expect("a player always has full access to their own data");
    }
}
