DROP INDEX IF EXISTS guilds_recruiting_idx;

ALTER TABLE guilds
    DROP COLUMN recruiting,
    DROP COLUMN links,
    DROP COLUMN banner,
    DROP COLUMN motd;
