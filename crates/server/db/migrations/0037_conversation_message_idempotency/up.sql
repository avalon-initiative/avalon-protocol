-- Idempotency key for conversation message submission (issue #111): the
-- deferred submission engine in avalon-sdk (crates/sdk/src/submission.rs)
-- retries a POST /conversations/{id}/messages request whenever it can't
-- tell whether a prior attempt's response was merely dropped in transit.
-- Without a server-side dedupe key, a retried request after a dropped
-- response would insert a second row for a message the game only ever
-- intended to send once.
--
-- client_entry_id carries the journal entry's client-generated `EntryId`
-- (crates/sdk/src/sync_journal.rs) straight through — nullable because a
-- message sent directly online (never queued through the journal /
-- submission engine) never sets one, and those never need to dedupe
-- against anything. The partial unique index (client_entry_id IS NOT NULL)
-- is what actually prevents a double-apply per (conversation,
-- client_entry_id) pair — same "unique constraint is what actually
-- prevents duplicates, the handler just discovers which case it's in"
-- pattern as `conversations.participants_key` (migration 0035) and
-- `blocks::create_block`'s unique-violation handling.
ALTER TABLE conversation_messages ADD COLUMN client_entry_id UUID;

CREATE UNIQUE INDEX conversation_messages_conversation_client_entry_idx
    ON conversation_messages (conversation_id, client_entry_id)
    WHERE client_entry_id IS NOT NULL;
