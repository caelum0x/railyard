-- Rithub self-serve provisioning (backlog item 4): a signed-in Console org
-- gets its own Gitea user account, a private org and a first repo with one
-- click, instead of a separate native Gitea signup. Nullable: orgs created
-- before this ship, or while GITEA_API_URL/GITEA_ADMIN_TOKEN are unset,
-- simply have none yet.

alter table railyard.orgs
  add column if not exists gitea_username text,
  add column if not exists gitea_org_slug text,
  add column if not exists gitea_repo_url text;

create index if not exists orgs_gitea_username_idx on railyard.orgs (gitea_username) where gitea_username is not null;
