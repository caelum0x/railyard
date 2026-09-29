-- AI gateway for Scribase projects (Wave P PY1.4): BYOK only (ADR RY-0003).
-- Idempotent. Owner applies it after 0002 (owner step); agents only to a
-- scratch Postgres.
--
-- ai_keys: one key per Scribase project environment, stored as a SHA-256.
-- ai_credentials: the customer's own provider keys, sealed with AES-256-GCM in
--   the console (RAILYARD_CREDENTIALS_KEY); the org and provider are bound as
--   associated data. Only a four-character hint is ever read back.
-- ai_usage: tokens per key per UTC day (the daily and monthly caps).
-- ai_traces: one row per call (model, provider, status, tokens, latency).
-- connect_tokens: one-time links to the secure connect page (15 minutes).

create table if not exists railyard.ai_keys (
  id                uuid primary key default gen_random_uuid(),
  org_id            uuid not null references railyard.orgs (id) on delete cascade,
  label             text not null check (length(label) between 1 and 80),
  key_hash          text not null unique,
  daily_token_cap   bigint not null check (daily_token_cap > 0),
  monthly_token_cap bigint not null check (monthly_token_cap > 0),
  created_at        timestamptz not null default now(),
  revoked_at        timestamptz
);
create index if not exists ai_keys_org_idx on railyard.ai_keys (org_id);

create table if not exists railyard.ai_credentials (
  org_id     uuid not null references railyard.orgs (id) on delete cascade,
  provider   text not null check (provider in ('openai', 'anthropic', 'groq', 'openrouter')),
  nonce      text not null,
  ciphertext text not null,
  key_hint   text not null,
  updated_at timestamptz not null default now(),
  primary key (org_id, provider)
);

create table if not exists railyard.ai_usage (
  key_id   uuid not null references railyard.ai_keys (id) on delete cascade,
  day      date not null,
  tokens   bigint not null default 0 check (tokens >= 0),
  requests integer not null default 0 check (requests >= 0),
  primary key (key_id, day)
);

create table if not exists railyard.ai_traces (
  id            bigint generated always as identity primary key,
  key_id        uuid not null references railyard.ai_keys (id) on delete cascade,
  at            timestamptz not null default now(),
  model         text not null,
  provider      text not null,
  status        integer not null,
  input_tokens  integer not null default 0,
  output_tokens integer not null default 0,
  latency_ms    integer not null default 0
);
create index if not exists ai_traces_key_at_idx on railyard.ai_traces (key_id, at desc);

create table if not exists railyard.connect_tokens (
  token_hash text primary key,
  org_id     uuid not null references railyard.orgs (id) on delete cascade,
  purpose    text not null check (purpose in ('ai')),
  expires_at timestamptz not null,
  used_at    timestamptz
);

-- Atomic usage increment for PostgREST (rpc/ai_add_usage).
create or replace function railyard.ai_add_usage(p_key uuid, p_day date, p_tokens bigint)
returns void language sql security invoker as $$
  insert into railyard.ai_usage (key_id, day, tokens, requests) values (p_key, p_day, greatest(p_tokens, 0), 1)
  on conflict (key_id, day) do update set tokens = railyard.ai_usage.tokens + greatest(excluded.tokens, 0),
                                          requests = railyard.ai_usage.requests + 1;
$$;

alter table railyard.ai_keys enable row level security;
alter table railyard.ai_credentials enable row level security;
alter table railyard.ai_usage enable row level security;
alter table railyard.ai_traces enable row level security;
alter table railyard.connect_tokens enable row level security;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    execute 'revoke all on railyard.ai_keys, railyard.ai_credentials, railyard.ai_usage, railyard.ai_traces, railyard.connect_tokens from anon';
    execute 'revoke all on function railyard.ai_add_usage(uuid, date, bigint) from anon';
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    execute 'revoke all on railyard.ai_keys, railyard.ai_credentials, railyard.ai_usage, railyard.ai_traces, railyard.connect_tokens from authenticated';
    execute 'revoke all on function railyard.ai_add_usage(uuid, date, bigint) from authenticated';
  end if;
  execute 'revoke all on function railyard.ai_add_usage(uuid, date, bigint) from public';
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    execute 'grant all on railyard.ai_keys, railyard.ai_credentials, railyard.ai_usage, railyard.ai_traces, railyard.connect_tokens to service_role';
    execute 'grant all on all sequences in schema railyard to service_role';
    execute 'grant execute on function railyard.ai_add_usage(uuid, date, bigint) to service_role';
  end if;
end $$;
