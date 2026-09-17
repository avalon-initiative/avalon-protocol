-- Issue #520: `observed_sths` only recorded `observed_at` (when this node
-- locally saw a peer's STH), never the STH's own `created_at` — part of
-- what its Ed25519 signature actually covers (see `crates/chain/src/sth.rs`'s
-- module doc comment). Re-serving a mirrored STH later (`GET /ledger/sth/*`
-- falling back to mirrored data) needs the real `created_at` to report, or
-- an independent caller re-verifying the signature against the pinned
-- network key would fail against a `created_at` this node never actually
-- observed being signed. Backfilled from `observed_at` for any row already
-- written before this migration (both were the same "just polled a peer"
-- code path before now, so this is the closest honest value available for
-- old rows, not a fabricated one) — see #520's ticket for context.
ALTER TABLE observed_sths ADD COLUMN created_at TIMESTAMPTZ;
UPDATE observed_sths SET created_at = observed_at WHERE created_at IS NULL;
ALTER TABLE observed_sths ALTER COLUMN created_at SET NOT NULL;
