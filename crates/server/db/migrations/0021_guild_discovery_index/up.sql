-- Issue #154: `GET /guilds/discover`'s `tag=` filter does a case-insensitive
-- exact match against `guilds.tag`. `guilds_recruiting_idx` (#153) already
-- covers the `recruiting` filter; this adds the matching index for `tag` so
-- the other cheap, exact-match filter doesn't force a sequential scan
-- either. The free-text `q=` search (`ILIKE '%...%'` over name/tag/
-- description) is deliberately NOT given a trigram index here — that's the
-- kind of real read-model/indexing work #42's indexer migration owns, not
-- something to bolt onto the milestone-1 `server`-side stand-in this ticket
-- documents itself as (see `docs/architecture/guilds.md`).
CREATE INDEX guilds_tag_lower_idx ON guilds (lower(tag));
