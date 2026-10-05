-- Flag targeting and percentage rollouts (Wave Q QY1.3, I10 and A19).
-- Rules and segments live in Railyard's tables for every flag environment,
-- whichever backend holds the on/off and value (table or the flags server),
-- so one hash and one rule engine answer everywhere. The rule documents are
-- validated by the application (lib/flags/targeting.mjs); the checks here keep
-- the shape and size bounded. Additive and idempotent. Not applied by agents
-- to any hosted database; the owner applies it after 0008.

create table if not exists railyard.flag_targeting (
  env_id     uuid not null references railyard.flag_envs (id) on delete cascade,
  key        text not null check (key ~ '^[a-z][a-z0-9_]{0,63}$'),
  targeting  jsonb not null check (jsonb_typeof(targeting) = 'object' and jsonb_typeof(targeting -> 'rules') = 'array'
                                   and jsonb_array_length(targeting -> 'rules') between 1 and 20
                                   and pg_column_size(targeting) <= 65536),
  updated_at timestamptz not null default now(),
  updated_by text check (updated_by is null or length(updated_by) <= 200),
  primary key (env_id, key)
);

create table if not exists railyard.flag_segments (
  env_id     uuid not null references railyard.flag_envs (id) on delete cascade,
  key        text not null check (key ~ '^[a-z][a-z0-9_]{0,63}$'),
  name       text not null check (length(name) between 1 and 80),
  conditions jsonb not null check (jsonb_typeof(conditions) = 'array' and jsonb_array_length(conditions) between 1 and 10
                                   and pg_column_size(conditions) <= 32768),
  updated_at timestamptz not null default now(),
  updated_by text check (updated_by is null or length(updated_by) <= 200),
  primary key (env_id, key)
);

alter table railyard.flag_targeting enable row level security;
alter table railyard.flag_segments enable row level security;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    execute 'revoke all on railyard.flag_targeting, railyard.flag_segments from anon';
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    execute 'revoke all on railyard.flag_targeting, railyard.flag_segments from authenticated';
  end if;
  if exists (select 1 from pg_roles where rolname = 'service_role') then
    execute 'grant all on railyard.flag_targeting, railyard.flag_segments to service_role';
  end if;
end $$;
