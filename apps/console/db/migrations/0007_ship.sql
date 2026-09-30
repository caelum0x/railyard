-- Ship on push (Wave P PY3.1, RYP-E3, ADR RY-0004): GitHub App installations,
-- linked repositories, the org's Cloudflare grant (sealed) and the short
-- `railyard link` browser sessions.
-- Idempotent: every statement can run again without error or change. Not
-- applied by agents to any hosted database; the owner applies it after 0006
-- (owner step), agents only to a scratch Postgres.

create table if not exists railyard.github_installations (
  installation_id bigint primary key check (installation_id > 0),
  org_id          uuid not null references railyard.orgs (id) on delete cascade,
  account_login   text not null check (account_login ~ '^[A-Za-z0-9][A-Za-z0-9-]{0,38}$'),
  created_at      timestamptz not null default now()
);
create index if not exists github_installations_org_idx on railyard.github_installations (org_id);

create table if not exists railyard.ship_links (
  id                uuid primary key default gen_random_uuid(),
  org_id            uuid not null references railyard.orgs (id) on delete cascade,
  app               text not null check (app ~ '^[a-z0-9][a-z0-9-]{0,39}$'),
  repo_id           bigint not null unique check (repo_id > 0),
  repo_full_name    text not null check (repo_full_name ~ '^[A-Za-z0-9][A-Za-z0-9-]{0,38}/[A-Za-z0-9._-]{1,100}$'),
  installation_id   bigint not null check (installation_id > 0),
  production_branch text not null check (length(production_branch) between 1 and 200),
  workflow_pr_url   text check (workflow_pr_url is null or workflow_pr_url ~ '^https://github\.com/'),
  created_at        timestamptz not null default now()
);
create index if not exists ship_links_org_idx on railyard.ship_links (org_id);
create index if not exists ship_links_installation_idx on railyard.ship_links (installation_id);

-- Sealed only: nonce + ciphertext (AES-256-GCM, org id as associated data).
create table if not exists railyard.cloudflare_connections (
  org_id             uuid primary key references railyard.orgs (id) on delete cascade,
  account_id         text not null check (account_id ~ '^[0-9a-f]{32}$'),
  account_name       text not null default '' check (length(account_name) <= 200),
  refresh_nonce      text not null,
  refresh_ciphertext text not null,
  access_nonce       text,
  access_ciphertext  text,
  access_expires_at  timestamptz,
  updated_at         timestamptz not null default now()
);

create table if not exists railyard.ship_link_sessions (
  id_hash         text primary key check (id_hash ~ '^[0-9a-f]{64}$'),
  org_id          uuid not null references railyard.orgs (id) on delete cascade,
  app             text not null check (app ~ '^[a-z0-9][a-z0-9-]{0,39}$'),
  repo_full_name  text not null,
  installation_id bigint,
  pkce_verifier   text not null check (length(pkce_verifier) between 43 and 128),
  state           text not null default 'started' check (state in ('started', 'github_done', 'linked', 'failed')),
  message         text check (message is null or length(message) <= 500),
  link_id         uuid references railyard.ship_links (id) on delete set null,
  expires_at      timestamptz not null,
  created_at      timestamptz not null default now()
);
create index if not exists ship_link_sessions_expires_idx on railyard.ship_link_sessions (expires_at);

alter table railyard.github_installations enable row level security;
alter table railyard.ship_links enable row level security;
alter table railyard.cloudflare_connections enable row level security;
alter table railyard.ship_link_sessions enable row level security;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    execute 'revoke all on railyard.github_installations, railyard.ship_links, railyard.cloudflare_connections, railyard.ship_link_sessions from anon';
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    execute 'revoke all on railyard.github_installations, railyard.ship_links, railyard.cloudflare_connections, railyard.ship_link_sessions from authenticated';
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    execute 'grant all on railyard.github_installations, railyard.ship_links, railyard.cloudflare_connections, railyard.ship_link_sessions to service_role';
  end if;
end $$;
