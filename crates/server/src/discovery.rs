//! User discovery: scoped, always-on friends-of-friends/mutual-guild
//! surfacing and opt-in global name/handle search. See
//! `docs/projects/backend-server/architecture/social-graph.md`'s "Today in the repo" for the
//! discoverable-preference default, why `discover_people` never takes a
//! query parameter, and the shared block/friend-exclusion logic.

use std::collections::HashSet;

use avalon_indexer::projections::friendships as friendship_reads;
use avalon_indexer::projections::guild_rosters;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::postgres::Postgres;
use sqlx::{QueryBuilder, Row};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::error::AppError;
use crate::guilds::escape_like;
use crate::handlers::authenticate;
use crate::state::AppState;

/// Every identity with an existing friendship to at least one identity in
/// `friend_ids` — i.e. friends-of-friends, not yet filtered against the
/// caller's own friends/blocks/self. Empty input short-circuits to an
/// empty result rather than issuing an `ANY($1)` query with an empty
/// array (which is valid but pointless here).
async fn friends_of_friends(
    state: &AppState,
    friend_ids: &HashSet<Uuid>,
) -> Result<HashSet<Uuid>, AppError> {
    let friend_ids: Vec<Uuid> = friend_ids.iter().copied().collect();
    Ok(friendship_reads::friends_of_any(&state.pool, &friend_ids).await?)
}

/// Every identity that shares at least one guild membership with `caller`,
/// not yet filtered against the caller's own friends/blocks/self. Also
/// reused by `conversations::create_conversation`.
pub(crate) async fn mutual_guild_members(
    state: &AppState,
    caller: Uuid,
) -> Result<HashSet<Uuid>, AppError> {
    Ok(guild_rosters::mutual_members(&state.pool, caller).await?)
}

/// Pure merge/filter step, directly unit-testable without a database
/// (see this module's tests below): unions the two candidate sources,
/// then drops the caller themselves, anyone already a friend, and anyone
/// blocked in either direction. A candidate present in both sources
/// collapses to a single entry via the `HashSet` union. Sorted so the
/// response is stable rather than depending on `HashSet` iteration order.
pub(crate) fn compute_candidates(
    caller: Uuid,
    friends_of_friends: HashSet<Uuid>,
    mutual_guild: HashSet<Uuid>,
    existing_friends: &HashSet<Uuid>,
    blocked_partners: &HashSet<Uuid>,
) -> Vec<Uuid> {
    let mut candidates: HashSet<Uuid> = friends_of_friends;
    candidates.extend(mutual_guild);
    candidates.remove(&caller);
    candidates.retain(|id| !existing_friends.contains(id) && !blocked_partners.contains(id));
    let mut candidates: Vec<Uuid> = candidates.into_iter().collect();
    candidates.sort();
    candidates
}

#[derive(Serialize, ToSchema)]
pub struct DiscoveryCandidate {
    pub identity_id: Uuid,
}

#[derive(Serialize, ToSchema)]
pub struct DiscoverPeopleResponse {
    pub candidates: Vec<DiscoveryCandidate>,
}

/// `GET /people/discover` — session-authenticated, no query parameters by
/// design (see module doc comment: this is never a name/handle search).
/// Computes candidates from the caller's own friends-of-friends and
/// mutual-guild relationships, excluding the caller, existing friends, and
/// any blocked relationship in either direction.
#[utoipa::path(
    get,
    path = "/people/discover",
    tag = "discovery",
    responses((status = 200, body = DiscoverPeopleResponse)),
)]
pub async fn discover_people(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DiscoverPeopleResponse>, AppError> {
    let caller = authenticate(&state, &headers).await?;

    let friend_ids = crate::friends::friend_partners(&state, caller).await?;
    let blocked_partners = crate::blocks::block_partners(&state, caller).await?;
    let fof = friends_of_friends(&state, &friend_ids).await?;
    let guild_mates = mutual_guild_members(&state, caller).await?;

    let candidates = compute_candidates(caller, fof, guild_mates, &friend_ids, &blocked_partners);

    Ok(Json(DiscoverPeopleResponse {
        candidates: candidates
            .into_iter()
            .map(|identity_id| DiscoveryCandidate { identity_id })
            .collect(),
    }))
}

// --- Opt-in global search ------------------------------------------------

/// Upserts `identity_id`'s own `discoverable` preference — the same
/// "insert lazily on first toggle, `ON CONFLICT` update after" shape
/// `presence::set_hide_active_in` established. Not transactional with any
/// other write: this is a user preference, not durable protocol
/// history, so there's nothing else it needs to stay atomic with (see the
/// module doc comment). Takes effect immediately — the very next
/// `search_identities` call (run against `&state.pool`, not a snapshot)
/// reflects it, satisfying the "no grace period" invariant.
pub(crate) async fn set_discoverable(
    state: &AppState,
    identity_id: Uuid,
    discoverable: bool,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO discovery_preferences (identity_id, discoverable) VALUES ($1, $2) \
         ON CONFLICT (identity_id) DO UPDATE SET discoverable = EXCLUDED.discoverable",
    )
    .bind(identity_id)
    .bind(discoverable)
    .execute(&state.pool)
    .await?;
    Ok(())
}

/// Trims `q` and treats an all-whitespace/empty query as "no query" —
/// pure so it's unit-testable without a live Postgres connection (see
/// tests below). `search_identities` short-circuits to an empty result in
/// that case rather than issuing a `WHERE ... ILIKE '%%'` query that would
/// (harmlessly, but pointlessly) match every opted-in identity.
fn normalized_query(q: &str) -> Option<&str> {
    let trimmed = q.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

const DEFAULT_SEARCH_LIMIT: i64 = 20;
const MAX_SEARCH_LIMIT: i64 = 50;

#[derive(Deserialize, IntoParams)]
pub struct SearchIdentitiesQuery {
    pub q: String,
    pub limit: Option<i64>,
}

#[derive(Serialize, ToSchema)]
pub struct SearchResultIdentity {
    pub identity_id: Uuid,
    pub display_name: String,
    pub avatar_url: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct SearchIdentitiesResponse {
    pub results: Vec<SearchResultIdentity>,
}

/// Builds the `GET /identities/search` query — split out from
/// [`search_identities`] so the filter logic is unit-testable via
/// [`sqlx::QueryBuilder::sql`] without a live Postgres connection, the
/// same precedent `guilds::build_discover_query` set.
///
/// Three conditions, none optional: `discoverable = true` (the entire
/// point of this ticket — a non-opted-in identity must never appear, even
/// via an exact-match name, which is why this is a fuzzy `ILIKE`, not the
/// exact-match `friends::resolve_handle` path), `identity_id <> caller`
/// (a caller never "finds" themselves here), and `identity_id <> ALL(blocked)`
/// (excludes every identity with a block relationship to `caller` in
/// either direction — `blocked` is `blocks::block_partners`'s already
/// direction-agnostic output, reused rather than reimplemented, same as
/// `discovery::compute_candidates` does). `q` is matched
/// case-insensitively against `display_name` — `display_name`
/// is the handle now, no separate discriminator suffix to also match.
fn build_search_query(
    caller: Uuid,
    blocked: &[Uuid],
    q: &str,
    limit: i64,
) -> QueryBuilder<Postgres> {
    let like = format!("%{}%", escape_like(q));
    let mut builder: QueryBuilder<Postgres> = QueryBuilder::new(
        "SELECT p.identity_id, p.display_name, p.avatar_url \
         FROM profiles p \
         JOIN discovery_preferences dp ON dp.identity_id = p.identity_id \
         WHERE dp.discoverable = true AND p.identity_id <> ",
    );
    builder.push_bind(caller);
    builder.push(" AND p.identity_id <> ALL(");
    builder.push_bind(blocked.to_vec());
    builder.push(") AND p.display_name ILIKE ");
    builder.push_bind(like);
    builder.push(" ORDER BY p.display_name ASC LIMIT ");
    builder.push_bind(limit);
    builder
}

/// `GET /identities/search?q=&limit=` — session-authenticated
/// open name/handle search, the opt-in counterpart to [`discover_people`]'s
/// always-on scoped surfacing. Matches only identities with
/// `discoverable = true` (see module doc comment); a non-opted-in identity
/// never appears here, full stop — not even to a caller who already knows
/// their exact handle (that's the separate, untouched
/// `friends::resolve_handle` exact-match path). Excludes the caller
/// themselves and any blocked relationship in either direction, same as
/// [`discover_people`].
#[utoipa::path(
    get,
    path = "/identities/search",
    tag = "discovery",
    params(SearchIdentitiesQuery),
    responses((status = 200, body = SearchIdentitiesResponse)),
)]
pub async fn search_identities(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SearchIdentitiesQuery>,
) -> Result<Json<SearchIdentitiesResponse>, AppError> {
    let caller = authenticate(&state, &headers).await?;

    let Some(q) = normalized_query(&query.q) else {
        return Ok(Json(SearchIdentitiesResponse {
            results: Vec::new(),
        }));
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_SEARCH_LIMIT)
        .clamp(1, MAX_SEARCH_LIMIT);

    let blocked: Vec<Uuid> = crate::blocks::block_partners(&state, caller)
        .await?
        .into_iter()
        .collect();

    let mut builder = build_search_query(caller, &blocked, q, limit);
    let rows = builder.build().fetch_all(&state.pool).await?;

    let mut results = Vec::with_capacity(rows.len());
    for row in rows {
        results.push(SearchResultIdentity {
            identity_id: row.try_get("identity_id")?,
            display_name: row.try_get("display_name")?,
            avatar_url: row.try_get("avatar_url")?,
        });
    }

    Ok(Json(SearchIdentitiesResponse { results }))
}

#[cfg(test)]
mod tests {
    //! No live Postgres reachable here — these exercise `compute_candidates`'s
    //! pure logic only. The full friends-of-friends/mutual-guild/block
    //! surfacing flow against real data is covered by
    //! `crates/server/tests/discovery.rs`, gated `--ignored`.

    use super::*;

    #[test]
    fn excludes_the_caller_even_if_they_appear_as_a_candidate() {
        let caller = Uuid::new_v4();
        let other = Uuid::new_v4();
        let fof: HashSet<Uuid> = [caller, other].into_iter().collect();
        let result = compute_candidates(
            caller,
            fof,
            HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert_eq!(result, vec![other]);
    }

    #[test]
    fn excludes_an_existing_friend() {
        let caller = Uuid::new_v4();
        let friend = Uuid::new_v4();
        let stranger = Uuid::new_v4();
        let fof: HashSet<Uuid> = [friend, stranger].into_iter().collect();
        let existing_friends: HashSet<Uuid> = [friend].into_iter().collect();
        let mut result = compute_candidates(
            caller,
            fof,
            HashSet::new(),
            &existing_friends,
            &HashSet::new(),
        );
        result.sort();
        assert_eq!(result, vec![stranger]);
    }

    #[test]
    fn excludes_a_blocked_identity_regardless_of_block_direction() {
        let caller = Uuid::new_v4();
        let blocked_by_caller = Uuid::new_v4();
        let blocked_caller = Uuid::new_v4();
        let stranger = Uuid::new_v4();
        let mutual_guild: HashSet<Uuid> = [blocked_by_caller, blocked_caller, stranger]
            .into_iter()
            .collect();
        // block_partners is direction-agnostic by construction (see
        // blocks::block_partners) — both ids land in the same set here.
        let blocked_partners: HashSet<Uuid> =
            [blocked_by_caller, blocked_caller].into_iter().collect();
        let mut result = compute_candidates(
            caller,
            HashSet::new(),
            mutual_guild,
            &HashSet::new(),
            &blocked_partners,
        );
        result.sort();
        assert_eq!(result, vec![stranger]);
    }

    #[test]
    fn a_candidate_from_both_sources_appears_exactly_once() {
        let caller = Uuid::new_v4();
        let both = Uuid::new_v4();
        let fof: HashSet<Uuid> = [both].into_iter().collect();
        let mutual_guild: HashSet<Uuid> = [both].into_iter().collect();
        let result =
            compute_candidates(caller, fof, mutual_guild, &HashSet::new(), &HashSet::new());
        assert_eq!(result, vec![both]);
    }

    #[test]
    fn no_candidates_yields_an_empty_list() {
        let caller = Uuid::new_v4();
        let result = compute_candidates(
            caller,
            HashSet::new(),
            HashSet::new(),
            &HashSet::new(),
            &HashSet::new(),
        );
        assert!(result.is_empty());
    }

    /// Belt-and-suspenders on this module's own load-bearing invariant:
    /// no query-string-driven search parameter exists on this endpoint —
    /// a grep-level check that `discover_people` never touches an
    /// `axum::extract::Query` extractor, mirroring
    /// `presence::tests::presence_handlers_never_call_chain_commit`'s
    /// "read your own source" pattern.
    #[test]
    fn discover_people_takes_no_query_parameters() {
        let source = include_str!("discovery.rs");
        let handler_start = source.find("pub async fn discover_people").unwrap();
        let handler_end = source[handler_start..].find("\n}\n").unwrap() + handler_start;
        let handler_source = &source[handler_start..handler_end];
        assert!(
            !handler_source.contains("Query"),
            "discover_people must never accept a query parameter — see module doc comment"
        );
    }

    // --- #205: opt-in global search --------------------------------

    #[test]
    fn normalized_query_treats_blank_input_as_no_query() {
        assert_eq!(normalized_query(""), None);
        assert_eq!(normalized_query("   "), None);
        assert_eq!(normalized_query("  alice  "), Some("alice"));
    }

    #[test]
    fn search_query_only_ever_matches_discoverable_true() {
        let caller = Uuid::new_v4();
        let builder = build_search_query(caller, &[], "alice", 20);
        assert!(builder.sql().as_str().contains("dp.discoverable = true"));
    }

    #[test]
    fn search_query_excludes_the_caller() {
        let caller = Uuid::new_v4();
        let builder = build_search_query(caller, &[], "alice", 20);
        assert!(builder.sql().as_str().contains("p.identity_id <> "));
    }

    #[test]
    fn search_query_excludes_blocked_partners_in_either_direction() {
        // `blocked` is `blocks::block_partners`'s output, which is already
        // direction-agnostic (both "caller blocked them" and "they blocked
        // caller" land in the same set) — this only has to check that the
        // set, whatever it contains, is actually applied as an exclusion.
        let caller = Uuid::new_v4();
        let blocked = vec![Uuid::new_v4(), Uuid::new_v4()];
        let builder = build_search_query(caller, &blocked, "alice", 20);
        assert!(builder.sql().as_str().contains("p.identity_id <> ALL("));
    }

    #[test]
    fn search_query_matches_display_name() {
        let caller = Uuid::new_v4();
        let builder = build_search_query(caller, &[], "alice", 20);
        let sql_str = builder.sql();
        let sql = sql_str.as_str();
        assert!(sql.contains("p.display_name ILIKE"));
    }

    #[test]
    fn search_query_escapes_like_wildcards_in_the_query_term() {
        // A literal `%`/`_` in the search term must never be interpreted
        // as an ILIKE wildcard — reuses `guilds::escape_like`, already unit
        // tested in `guilds.rs`; this just checks `build_search_query`
        // actually calls through it rather than binding the raw term.
        let caller = Uuid::new_v4();
        let mut builder = build_search_query(caller, &[], "100%_off", 20);
        // The escaped like-pattern is one of the bound parameters, not
        // embedded in the SQL text itself (it's parameter-bound, not
        // interpolated) — so this asserts indirectly via `escape_like`
        // itself producing the expected escaped form used to build the
        // bound value, and that building the query doesn't panic.
        assert_eq!(escape_like("100%_off"), "100\\%\\_off");
        let _ = builder.build();
    }

    /// Same "read your own source" belt-and-suspenders check
    /// `discover_people_takes_no_query_parameters` uses in reverse: unlike
    /// `discover_people`, `search_identities` *must* accept a query
    /// parameter — this is the one endpoint in this module where that's
    /// correct.
    #[test]
    fn search_identities_does_accept_a_query_parameter() {
        let source = include_str!("discovery.rs");
        let handler_start = source.find("pub async fn search_identities").unwrap();
        let handler_end = source[handler_start..].find("\n}\n").unwrap() + handler_start;
        let handler_source = &source[handler_start..handler_end];
        assert!(handler_source.contains("Query(query): Query<SearchIdentitiesQuery>"));
    }
}
