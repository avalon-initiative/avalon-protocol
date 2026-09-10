-- Additive integrator category on `games` (issue #282, decision #275).
-- Defaults every existing row (and every caller that omits it) to 'game'.
ALTER TABLE games
    ADD COLUMN category TEXT NOT NULL DEFAULT 'game';
