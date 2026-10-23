-- Console as an OIDC provider (backlog item 5, closes apps/gitea's
-- TODO(railyard-sso)): single-use authorization codes for the authorization
-- code grant served at /oidc/authorize + /oidc/token. Idempotent; not
-- applied by agents to any hosted database (owner step), agents only run it
-- against a scratch Postgres.

create table if not exists railyard.oidc_codes (
  code_hash    text primary key,
  client_id    text not null check (length(client_id) between 1 and 100),
  redirect_uri text not null check (redirect_uri ~ '^https://' and length(redirect_uri) <= 500),
  org_id       uuid not null references railyard.orgs (id) on delete cascade,
  user_id      text not null check (length(user_id) between 1 and 200),
  email        text not null check (length(email) between 3 and 254),
  nonce        text check (nonce is null or length(nonce) <= 200),
  scope        text not null check (length(scope) between 1 and 200),
  created_at   timestamptz not null default now(),
  expires_at   timestamptz not null,
  consumed_at  timestamptz
);
create index if not exists oidc_codes_expires_idx on railyard.oidc_codes (expires_at);

alter table railyard.oidc_codes enable row level security;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    execute 'revoke all on railyard.oidc_codes from anon';
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    execute 'revoke all on railyard.oidc_codes from authenticated';
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    execute 'grant all on railyard.oidc_codes to service_role';
  end if;
end $$;
