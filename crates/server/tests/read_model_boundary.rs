//! Issue #44's guard: a plain source-text check, not `--ignored` — no live
//! infra needed, so this always runs as part of `make test`/CI, not just
//! `make test-live`.
//!
//! The invariant: `handlers.rs` reads current state through
//! `avalon_indexer::projections::*`, never by querying a projection table
//! (`profiles`) directly. Writes still go through
//! `PostgresIndexer::apply_in_tx`/the outbox — this only guards reads.
//!
//! One deliberate, named, out-of-scope exception this test does not try to
//! catch: `handlers::my_history` reads the ledger directly via
//! `PostgresSettlementProvider::list_entries_for_issuer_prefix`, since it
//! serves the raw historical log itself (a settlement-native read), not
//! current derived state — see that function's own doc comment for why.
//! Reliably distinguishing "the one allowed call" from "a new illicit one"
//! by source text alone is brittle against reformatting, so this guard
//! sticks to what it can check robustly: no handler queries the
//! `ledger_entries` table or the `profiles` projection table by name.

const HANDLERS_SRC: &str = include_str!("../src/handlers.rs");

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
