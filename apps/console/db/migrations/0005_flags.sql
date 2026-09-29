-- Railyard Flags (Wave P PY2.1, RYP-E2, SP-E6). Idempotent: every statement can
-- run again without error or change. Not applied by agents to any hosted
-- database; the owner applies it after 0004 (owner step), agents only to a
-- scratch Postgres.
--
-- flag_envs: one row per app environment (a Scribase project environment or
-- branch). The client key is public (it ships in the app) and reads that one
-- environment only. With the Flagsmith backend, backend_* hold the Flagsmith
-- organisation, project and environment key; with the table backend (the
-- fallback) the values live in railyard.flags.

create table if not exists railyard.flag_envs (
  id              uuid primary key default gen_random_uuid(),
  org_id          uuid not null references railyard.orgs (id) on delete cascade,
  app             text not null check (app ~ '^[a-z0-9][a-z0-9-]{0,39}$'),
  env             text not null check (env ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$'),
  client_key      text not null unique check (client_key ~ '^ry_flags_[A-Za-z0-9_-]{24,64}$'),
  backend         text not null default 'table' check (backend in ('table', 'flagsmith')),
  backend_ref     text check (backend_ref is null or length(backend_ref) between 1 and 200),
  backend_project text check (backend_project is null or length(backend_project) between 1 and 64),
  backend_org     text check (backend_org is null or length(backend_org) between 1 and 64),
  copied_from     uuid references railyard.flag_envs (id) on delete set null,
  first_read_at   timestamptz,
  created_at      timestamptz not null default now(),
  unique (org_id, app, env)
);
create index if not exists flag_envs_org_idx on railyard.flag_envs (org_id);

create table if not exists railyard.flags (
  env_id      uuid not null references railyard.flag_envs (id) on delete cascade,
  key         text not null check (key ~ '^[a-z][a-z0-9_]{0,63}$'),
  enabled     boolean not null default false,
  value       text check (value is null or length(value) <= 2000),
  description text check (description is null or length(description) <= 200),
  updated_at  timestamptz not null default now(),
  updated_by  text check (updated_by is null or length(updated_by) <= 200),
  primary key (env_id, key)
);

alter table railyard.flag_envs enable row level security;
alter table railyard.flags enable row level security;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    execute 'revoke all on railyard.flag_envs, railyard.flags from anon';
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    execute 'revoke all on railyard.flag_envs, railyard.flags from authenticated';
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    execute 'grant all on railyard.flag_envs, railyard.flags to service_role';
  end if;
end $$;
