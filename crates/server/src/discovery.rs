//! Scoped player discovery: friends-of-friends and mutual-guild surfacing
//! (issue #204), implementing the always-on half of #129's decided shape
//! (`docs/architecture/social-graph.md`). The other half — an opt-in,
//! reversible "make me publicly searchable" toggle that enables open
//! name/handle search — is issue #205, a separate ticket, not built here.
//!
//! **Never a search.** The only input to [`discover_people`] is the
//! caller's own session — there is no query parameter, and there must
//! never be one added to this endpoint. Candidates come exclusively from
//! relationships that already exist:
//!
//!   * friends-of-friends — identities with an existing friendship to at
//!     least one of the caller's own friends;
//!   * mutual guild membership — identities who are members of at least
//!     one guild the caller is also a member of.
//!
//! Both sources exclude the caller themselves, anyone already a friend of
//! the caller, and anyone with a block relationship to the caller in
//! either direction — reusing `friends::friend_partners` and
//! `blocks::block_partners` exactly, the same "compute related identities"
//! precedent `presence.rs` established for issue #16, rather than
//! reimplementing either check. A candidate reachable via both sources
//! appears once ([`compute_candidates`] dedupes through a `HashSet`).
//!
//! Milestone-1 stand-in, matching #154's `guilds::build_discover_query`
//! precedent: a direct query over `friendships`/`guild_members`, not the
//! real indexer read model. Read-only — this never creates or modifies a
//! friendship; acting on a suggestion still goes through
//! `POST /friends/requests` (#15).

use std::collections::HashSet;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::error::AppError;
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
    if friend_ids.is_empty() {
        return Ok(HashSet::new());
    }
    let friend_ids: Vec<Uuid> = friend_ids.iter().copied().collect();
    let rows = sqlx::query(
        r#"
        SELECT b AS candidate FROM friendships WHERE a = ANY($1)
        UNION
        SELECT a AS candidate FROM friendships WHERE b = ANY($1)
        "#,
    )
    .bind(&friend_ids)
    .fetch_all(&state.pool)
    .await?;
    let mut set = HashSet::with_capacity(rows.len());
    for row in rows {
        set.insert(row.try_get("candidate")?);
    }
    Ok(set)
}

/// Every identity that shares at least one guild membership with `caller`,
/// not yet filtered against the caller's own friends/blocks/self.
async fn mutual_guild_members(state: &AppState, caller: Uuid) -> Result<HashSet<Uuid>, AppError> {
    let rows = sqlx::query(
        r#"
        SELECT DISTINCT gm2.identity_id AS candidate
        FROM guild_members gm1
        JOIN guild_members gm2 ON gm2.guild_id = gm1.guild_id
        WHERE gm1.identity_id = $1 AND gm2.identity_id != $1
        "#,
    )
    .bind(caller)
    .fetch_all(&state.pool)
    .await?;
    let mut set = HashSet::with_capacity(rows.len());
    for row in rows {
        set.insert(row.try_get("candidate")?);
    }
    Ok(set)
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

#[derive(Serialize)]
pub struct DiscoveryCandidate {
    pub identity_id: Uuid,
}

#[derive(Serialize)]
pub struct DiscoverPeopleResponse {
    pub candidates: Vec<DiscoveryCandidate>,
}

/// `GET /people/discover` — session-authenticated, no query parameters by
/// design (see module doc comment: this is never a name/handle search).
/// Computes candidates from the caller's own friends-of-friends and
/// mutual-guild relationships, excluding the caller, existing friends, and
/// any blocked relationship in either direction.
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
}
