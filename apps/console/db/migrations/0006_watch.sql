-- Railyard Watch (Wave P PY2.2, RYP-E4): uptime checks and public status pages.
-- Idempotent: every statement can run again without error or change. Not
-- applied by agents to any hosted database; the owner applies it after 0005
-- (owner step), agents only to a scratch Postgres.
--
-- watch_checks: one public https URL of one app; slug names its public status
-- page. watch_status: the latest state, the last 90 results and the state the
-- owner was last emailed about (one email per change).

create table if not exists railyard.watch_checks (
  id          uuid primary key default gen_random_uuid(),
  org_id      uuid not null references railyard.orgs (id) on delete cascade,
  app         text not null check (app ~ '^[a-z0-9][a-z0-9-]{0,39}$'),
  name        text not null check (length(name) between 1 and 60),
  url         text not null check (url ~ '^https://' and length(url) <= 500),
  slug        text not null unique check (slug ~ '^[a-z0-9][a-z0-9-]{2,60}$'),
  owner_email text check (owner_email is null or (length(owner_email) <= 254 and position('@' in owner_email) > 1)),
  paused      boolean not null default false,
  created_at  timestamptz not null default now(),
  unique (org_id, app, url)
);
create index if not exists watch_checks_org_idx on railyard.watch_checks (org_id);
create index if not exists watch_checks_active_idx on railyard.watch_checks (created_at) where not paused;

create table if not exists railyard.watch_status (
  check_id             uuid primary key references railyard.watch_checks (id) on delete cascade,
  state                text not null default 'waiting' check (state in ('waiting', 'up', 'down')),
  consecutive_failures integer not null default 0 check (consecutive_failures >= 0),
  first_result_at      timestamptz,
  last_checked_at      timestamptz,
  last_change_at       timestamptz,
  alerted_state        text check (alerted_state is null or alerted_state in ('waiting', 'up', 'down')),
  history              jsonb not null default '[]'::jsonb check (jsonb_typeof(history) = 'array' and jsonb_array_length(history) <= 90)
);

alter table railyard.watch_checks enable row level security;
alter table railyard.watch_status enable row level security;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    execute 'revoke all on railyard.watch_checks, railyard.watch_status from anon';
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    execute 'revoke all on railyard.watch_checks, railyard.watch_status from authenticated';
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    execute 'grant all on railyard.watch_checks, railyard.watch_status to service_role';
  end if;
end $$;
