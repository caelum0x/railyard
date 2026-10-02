-- Ship on push (QY1.1, P-Y11): the browser session keeps the repository id
-- GitHub confirmed when the linking user was checked. finish() looks the
-- repository up by this id, so a rename or a look-alike name cannot be linked
-- in its place. Additive: 0007 is unchanged. Idempotent. Not applied by
-- agents to any hosted database; the owner applies it after 0007.

alter table railyard.ship_link_sessions add column if not exists repo_id bigint check (repo_id is null or repo_id > 0);
