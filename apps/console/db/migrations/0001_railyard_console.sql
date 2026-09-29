-- Railyard console schema (RYP-D1, Wave E package Y-2).
-- Lives in the Railyard console's own Scribase project (dogfood), schema
-- `railyard`. Idempotent: every statement can run again without error or
-- change. Not applied by agents to any hosted database; the owner applies it
-- to the Scribase project (owner step), agents only to a scratch Postgres.
--
-- Access: the console server talks to these tables with the project's service
-- role only. RLS is on with no policies, so anon and authenticated keys read
-- and write nothing.

create schema if not exists railyard;

create table if not exists railyard.orgs (
  id            uuid primary key default gen_random_uuid(),
  name          text not null check (length(name) between 1 and 80),
  owner_user_id uuid not null,
  created_at    timestamptz not null default now()
);
create unique index if not exists orgs_owner_user_id_key on railyard.orgs (owner_user_id);

create table if not exists railyard.org_members (
  org_id     uuid not null references railyard.orgs (id) on delete cascade,
  user_id    uuid not null,
  email      text not null,
  role       text not null default 'owner' check (role in ('owner', 'member')),
  created_at timestamptz not null default now(),
  primary key (org_id, user_id)
);
create index if not exists org_members_user_id_idx on railyard.org_members (user_id);

create table if not exists railyard.apps (
  id         uuid primary key default gen_random_uuid(),
  org_id     uuid not null references railyard.orgs (id) on delete cascade,
  name       text not null check (name ~ '^[a-z0-9][a-z0-9-]{0,39}$'),
  site_key   text not null unique,
  created_at timestamptz not null default now(),
  unique (org_id, name)
);

create table if not exists railyard.deploys (
  id          uuid primary key default gen_random_uuid(),
  org_id      uuid not null references railyard.orgs (id) on delete cascade,
  app_id      uuid not null references railyard.apps (id) on delete cascade,
  url         text not null,
  commit_sha  text,
  branch      text not null,
  framework   text not null check (framework in ('nextjs', 'vite', 'astro', 'static')),
  version_id  text,
  duration_ms integer check (duration_ms >= 0),
  status      text not null check (status in ('running', 'succeeded', 'failed', 'rolled_back')),
  rollback_of uuid references railyard.deploys (id),
  created_at  timestamptz not null default now()
);
create index if not exists deploys_app_created_idx on railyard.deploys (app_id, created_at desc);

create table if not exists railyard.entitlements (
  org_id               uuid primary key references railyard.orgs (id) on delete cascade,
  plan                 text not null default 'none' check (plan in ('none', 'trial', 'railyard')),
  status               text not null default 'inactive'
                         check (status in ('inactive', 'trialing', 'active', 'paused', 'cancelled')),
  interval             text check (interval in ('month', 'year')),
  apps_limit           integer not null default 0 check (apps_limit >= 0),
  events_limit         integer not null default 0 check (events_limit >= 0),
  credit_cents         integer not null default 0,
  card_verified_at     timestamptz,
  card_check_payment_id text,
  trial_ends_at        timestamptz,
  dodo_customer_id     text,
  subscription_id      text,
  paused               boolean not null default false,
  cancel_at_period_end boolean not null default false,
  updated_at           timestamptz not null default now()
);

create table if not exists railyard.billing_events (
  id          bigint generated always as identity primary key,
  webhook_id  text not null,
  org_id      uuid references railyard.orgs (id) on delete set null,
  event_type  text not null,
  payment_id  text,
  kind        text,
  amount_cents integer,
  outcome     text not null,
  received_at timestamptz not null default now()
);
-- Replay protection: one row per Standard Webhooks message id.
create unique index if not exists billing_events_webhook_id_key on railyard.billing_events (webhook_id);
create index if not exists billing_events_payment_idx on railyard.billing_events (payment_id);

-- CLI tokens (device flow). Only a SHA-256 of the token is stored.
create table if not exists railyard.api_tokens (
  id          uuid primary key default gen_random_uuid(),
  org_id      uuid not null references railyard.orgs (id) on delete cascade,
  user_id     uuid not null,
  token_hash  text not null unique,
  label       text not null default 'cli',
  created_at  timestamptz not null default now(),
  last_used_at timestamptz,
  revoked_at  timestamptz
);

create table if not exists railyard.device_codes (
  device_code_hash text primary key,
  user_code        text not null unique,
  org_id           uuid references railyard.orgs (id) on delete cascade,
  user_id          uuid,
  approved_at      timestamptz,
  consumed_at      timestamptz,
  expires_at       timestamptz not null,
  created_at       timestamptz not null default now()
);

alter table railyard.orgs enable row level security;
alter table railyard.org_members enable row level security;
alter table railyard.apps enable row level security;
alter table railyard.deploys enable row level security;
alter table railyard.entitlements enable row level security;
alter table railyard.billing_events enable row level security;
alter table railyard.api_tokens enable row level security;
alter table railyard.device_codes enable row level security;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    execute 'revoke all on all tables in schema railyard from anon';
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    execute 'revoke all on all tables in schema railyard from authenticated';
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    execute 'grant usage on schema railyard to service_role';
    execute 'grant all on all tables in schema railyard to service_role';
    execute 'grant all on all sequences in schema railyard to service_role';
  end if;
end $$;
