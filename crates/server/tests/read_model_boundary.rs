//! Issue #44's guard, extended by #506 to `friends.rs`/`guilds.rs`: a plain
//! source-text check, not `--ignored` — no live infra needed, so this
//! always runs as part of `make test`/CI, not just `make test-live`.
//!
//! The invariant: request handlers read current state through
//! `avalon_indexer::projections::*`, never by querying a projection table
//! (`profiles`, `indexer_friendships`, `indexer_guild_members`) directly.
//! Writes still go through `PostgresIndexer::apply_in_tx`/the outbox — this
//! only guards reads.
//!
//! One deliberate, named, out-of-scope exception this test does not try to
//! catch: `handlers::my_history` reads the ledger directly via
//! `PostgresSettlementProvider::list_entries_for_issuer_prefix`, since it
//! serves the raw historical log itself (a settlement-native read), not
//! current derived state — see that function's own doc comment for why.
//! Reliably distinguishing "the one allowed call" from "a new illicit one"
//! by source text alone is brittle against reformatting, so this guard
//! sticks to what it can check robustly: no handler queries the
//! `ledger_entries` table or a projection table by name.

const HANDLERS_SRC: &str = include_str!("../src/handlers.rs");
const FRIENDS_SRC: &str = include_str!("../src/friends.rs");
const GUILDS_SRC: &str = include_str!("../src/guilds.rs");

#[test]
fn handlers_do_not_query_ledger_entries_directly() {
    assert!(
        !HANDLERS_SRC.contains("ledger_entries"),
        "handlers.rs must never query the ledger_entries table directly"
    );
}

#[test]
fn handlers_do_not_query_the_profiles_projection_table_directly() {
    for needle in ["FROM profiles", "INTO profiles", "UPDATE profiles"] {
        assert!(
            !HANDLERS_SRC.contains(needle),
            "handlers.rs must not query `profiles` directly ({needle}) — reads go through \
             avalon_indexer::projections::profiles, writes through PostgresIndexer::apply_in_tx"
        );
    }
}

/// Issue #506: `friends.rs` no longer writes or reads the old `friendships`
/// table directly — `avalon_indexer::projections::friendships` owns it
/// entirely now (`indexer_friendships`). `friend_requests` (pending,
/// not-yet-accepted requests) is untouched by this guard on purpose — it's
/// workflow state, never in this projection's scope (see that module's own
/// doc comment).
#[test]
fn friends_do_not_query_the_friendships_projection_table_directly() {
    for needle in [
        "FROM friendships",
        "INTO friendships",
        "UPDATE friendships",
        "DELETE FROM friendships",
    ] {
        assert!(
            !FRIENDS_SRC.contains(needle),
            "friends.rs must not query `friendships` directly ({needle}) — reads go through \
             avalon_indexer::projections::friendships, writes through PostgresIndexer::apply_in_tx"
        );
    }
}

/// Issue #506: `guilds.rs` no longer writes or reads the old `guild_members`
/// table directly — `avalon_indexer::projections::guild_rosters` owns it
/// entirely now (`indexer_guild_members`). `guild_roles` (role definitions/
/// permissions) is untouched by this guard on purpose — it's server-owned
/// data, never part of this projection's scope.
#[test]
fn guilds_do_not_query_the_guild_members_projection_table_directly() {
    for needle in [
        "FROM guild_members",
        "INTO guild_members",
        "UPDATE guild_members",
        "DELETE FROM guild_members",
    ] {
        assert!(
            !GUILDS_SRC.contains(needle),
            "guilds.rs must not query `guild_members` directly ({needle}) — reads go through \
             avalon_indexer::projections::guild_rosters, writes through PostgresIndexer::apply_in_tx"
        );
    }
}
