//! Issue #102 invariant: nothing under the conversations path
//! (`src/conversations.rs`) touches the ledger or the outbox — conversation
//! messages are deliberately not protocol history (see that module's doc
//! comment, and `crate::channels`/`crate::guilds` for the contrasting
//! "durable history" case). A plain source grep, run as a normal
//! `#[test]` (no live Postgres needed, so this isn't `--ignored`) — same
//! approach `guild_messages_no_ledger.rs` established for issue #22.
//!
//! This lives in its own file, separate from `src/conversations.rs`,
//! deliberately: a test can't grep its own file for the absence of a
//! string without that string then being present in the file, which would
//! make the check permanently fail against itself.

#[test]
fn conversations_module_never_references_ledger_or_outbox() {
    let source = include_str!("../src/conversations.rs");

    assert!(
        !source.contains("SettlementProvider"),
        "src/conversations.rs must never reference SettlementProvider"
    );
    assert!(
        !source.contains("avalon_chain::"),
        "src/conversations.rs must never import avalon_chain"
    );
    assert!(
        !source.contains("outbox::enqueue("),
        "src/conversations.rs must never call outbox::enqueue"
    );
    assert!(
        !source.contains("use crate::outbox"),
        "src/conversations.rs must never import crate::outbox"
    );
    assert!(
        !source.contains("state.chain.commit"),
        "src/conversations.rs must never call state.chain.commit"
    );
    assert!(
        !source.contains("\"conversation.message_"),
        "src/conversations.rs must never define a conversation.message_* protocol event kind"
    );
}
