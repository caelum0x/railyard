-- Scribase bridge (Wave P PY1.0, RYP-E6) and the portfolio add-on plan (PY1.1).
-- Idempotent: every statement can run again without error or change. Not
-- applied by agents to any hosted database; the owner applies it after 0001
-- (owner step), agents only to a scratch Postgres.
--
-- A Scribase org that turns on the portfolio add-on gets one Railyard org,
-- created by the bridge on first use and bound here. Scribase bills the
-- add-on; the entitlement row mirrors it with plan 'addon'.

create table if not exists railyard.scribase_bindings (
  scribase_org_id text primary key check (scribase_org_id ~ '^[A-Za-z0-9_-]{1,128}$'),
  org_id          uuid not null unique references railyard.orgs (id) on delete cascade,
  created_at      timestamptz not null default now()
);

-- Replay protection for signed bridge calls: one row per Standard Webhooks id.
create table if not exists railyard.bridge_messages (
  message_id  text primary key check (length(message_id) between 1 and 200),
  received_at timestamptz not null default now()
);
create index if not exists bridge_messages_received_idx on railyard.bridge_messages (received_at);

alter table railyard.entitlements add column if not exists addon_slots text[] not null default '{}';
alter table railyard.entitlements add column if not exists addon_renews_at timestamptz;

-- Widen the plan check to the add-on plan (same name, so a re-run replaces it).
alter table railyard.entitlements drop constraint if exists entitlements_plan_check;
alter table railyard.entitlements
  add constraint entitlements_plan_check check (plan in ('none', 'trial', 'railyard', 'addon'));

alter table railyard.scribase_bindings enable row level security;
alter table railyard.bridge_messages enable row level security;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    execute 'revoke all on railyard.scribase_bindings, railyard.bridge_messages from anon';
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    execute 'revoke all on railyard.scribase_bindings, railyard.bridge_messages from authenticated';
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    execute 'grant all on railyard.scribase_bindings, railyard.bridge_messages to service_role';
  end if;
end $$;
