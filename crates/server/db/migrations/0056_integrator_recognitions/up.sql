-- Public recognition relationships (issue #89, decided by #290's own
-- generalized vocabulary) — one integrator publishing "I recognize
-- <other integrator>'s claims, for <scope>" as a durable fact, not a
-- score or a boolean. See docs/architecture/registry.md's "Recognition
-- relationships and the network graph" section.
--
-- `recognizer_id`/`recognized_id` are ordered (who recognizes whom), not
-- symmetric — Integrator A recognizing B says nothing about whether B
-- recognizes A. `scope` is a free-form text array (e.g.
-- `{achievements,tournament_results}`), never a fixed enum: what an
-- integrator chooses to recognize another for is its own policy
-- statement, not a protocol-defined vocabulary.
--
-- Upserted, not append-only: republishing updates `scope`/`published_at`
-- in place and clears `revoked_at`; revoking sets `revoked_at` without
-- deleting the row, so "A used to recognize B, then stopped" stays a
-- fact a reader can see, not silently erased. Mirrors
-- `integrator_schemas`/`indexer_integrator_schemas`'s own
-- server-table-plus-indexer-projection split.
CREATE TABLE integrator_recognitions (
    recognizer_id UUID NOT NULL REFERENCES integrators(id) ON DELETE CASCADE,
    recognized_id UUID NOT NULL REFERENCES integrators(id) ON DELETE CASCADE,
    scope TEXT[] NOT NULL,
    published_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    revoked_at TIMESTAMPTZ,
    PRIMARY KEY (recognizer_id, recognized_id)
);

CREATE INDEX integrator_recognitions_recognized_id_idx
    ON integrator_recognitions (recognized_id);

CREATE TABLE indexer_integrator_recognitions (
    recognizer_id UUID NOT NULL,
    recognized_id UUID NOT NULL,
    scope TEXT[] NOT NULL,
    published_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ,
    PRIMARY KEY (recognizer_id, recognized_id)
);

CREATE INDEX indexer_integrator_recognitions_recognized_id_idx
    ON indexer_integrator_recognitions (recognized_id);
