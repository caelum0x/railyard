-- Custom domains for shipped apps (Wave Q QY1.5, I6). One row per hostname
-- attached to an app's production Worker on the org's own Cloudflare account.
-- The console keeps what is attached and its certificate state for the
-- Scribase Deploy tab; no credential is stored here. Additive and
-- idempotent. Not applied by agents to any hosted database; the owner
-- applies it after 0009.

create table if not exists railyard.app_domains (
  org_id       uuid not null references railyard.orgs (id) on delete cascade,
  app          text not null check (app ~ '^[a-z0-9][a-z0-9-]{0,39}$'),
  hostname     text not null check (length(hostname) between 4 and 253 and hostname = lower(hostname)),
  worker       text not null check (length(worker) between 1 and 63),
  cf_domain_id text check (cf_domain_id is null or length(cf_domain_id) <= 64),
  zone_name    text check (zone_name is null or length(zone_name) <= 253),
  state        text not null default 'pending' check (state in ('pending', 'active', 'error')),
  checked_at   timestamptz,
  created_at   timestamptz not null default now(),
  primary key (org_id, hostname)
);
create index if not exists app_domains_app_idx on railyard.app_domains (org_id, app);

alter table railyard.app_domains enable row level security;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    execute 'revoke all on railyard.app_domains from anon';
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    execute 'revoke all on railyard.app_domains from authenticated';
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    execute 'grant all on railyard.app_domains to service_role';
  end if;
end $$;
