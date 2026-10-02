-- Records which node inserted each replica row so a delete applies only to its own rows.
-- Rows that predate the column get '', which matches no peer id, so no peer can delete them.
SET LOCAL lock_timeout = '10s';
ALTER TABLE guild_messages_replica ADD COLUMN replicated_by TEXT NOT NULL DEFAULT '';
ALTER TABLE guild_messages_replica ALTER COLUMN replicated_by DROP DEFAULT;
ALTER TABLE conversation_messages_replica ADD COLUMN replicated_by TEXT NOT NULL DEFAULT '';
ALTER TABLE conversation_messages_replica ALTER COLUMN replicated_by DROP DEFAULT;
