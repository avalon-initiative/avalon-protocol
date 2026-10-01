ALTER TABLE protocol_outbox DROP COLUMN seq;
DROP SEQUENCE IF EXISTS protocol_outbox_seq_seq;
