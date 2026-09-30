-- Experiments (A3) on the flags slot. The flag itself lives in the flags
-- slot and is only ever changed through its own service; this table keeps
-- the experiment's plan, its metric and its state. Idempotent. Row level
-- security in 0024.

create table if not exists pa.experiments (
  id          uuid        primary key default gen_random_uuid(),
  project_id  uuid        not null,
  name        text        not null check (length(name) between 1 and 200),
  description text        not null default '' check (length(description) <= 1000),
  flag_key    text        not null check (flag_key ~ '^[a-z][a-z0-9_]{0,63}$'),
  flags_env   text        not null check (flags_env ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$'),
  variants    jsonb       not null check (jsonb_typeof(variants) = 'array'),
  rollout     integer     not null default 100 check (rollout between 0 and 100),
  metric      jsonb       not null check (jsonb_typeof(metric) = 'object'),
  method      text        not null default 'bayesian' check (method in ('bayesian', 'frequentist')),
  status      text        not null default 'draft' check (status in ('draft', 'running', 'stopped')),
  started_at  timestamptz,
  stopped_at  timestamptz,
  created_at  timestamptz not null default now(),
  updated_at  timestamptz not null default now(),
  unique (project_id, name)
);
-- One running experiment per flag.
create unique index if not exists experiments_running_flag_idx on pa.experiments (project_id, flag_key) where status = 'running';
