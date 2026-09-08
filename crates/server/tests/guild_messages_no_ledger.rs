//! Issue #22 invariant: nothing in the guild-message path (`src/guild_messages.rs`)
//! touches the ledger or the outbox — chat messages are deliberately not
//! protocol history (see that module's doc comment, and
//! `crate::channels`/`crate::guilds` for the contrasting "durable history"
//! case). A plain source grep, run as a normal `#[test]` (no live Postgres
//! needed, so this isn't `--ignored`).
//!
//! This lives in its own file, separate from `src/guild_messages.rs`,
//! deliberately: a test can't grep its own file for the absence of a
//! string without that string then being present in the file, which would
//! make the check permanently fail against itself.

#[test]
fn guild_messages_module_never_references_ledger_or_outbox() {
    let source = include_str!("../src/guild_messages.rs");

    assert!(
        !source.contains("SettlementProvider"),
        "src/guild_messages.rs must never reference SettlementProvider"
    );
    assert!(
        !source.contains("avalon_chain::"),
        "src/guild_messages.rs must never import avalon_chain"
    );
    assert!(
        !source.contains("outbox::enqueue("),
        "src/guild_messages.rs must never call outbox::enqueue"
    );
    assert!(
        !source.contains("use crate::outbox"),
        "src/guild_messages.rs must never import crate::outbox"
    );
    assert!(
        !source.contains("\"guild.message_"),
        "src/guild_messages.rs must never define a guild.message_* protocol event kind"
    );
}
