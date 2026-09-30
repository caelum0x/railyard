-- Exports (A3): destinations (webhook, S3) with a cursor over
-- (inserted_at, uuid) of pa.events, and a run log. The destination's secret
-- (webhook signing secret or S3 secret key) is stored encrypted with
-- AES-256-GCM under PA_EXPORTS_KEY. Idempotent. Row level security in 0024.

create table if not exists pa.export_destinations (
  id                 uuid        primary key default gen_random_uuid(),
  project_id         uuid        not null,
  name               text        not null check (length(name) between 1 and 200),
  kind               text        not null check (kind in ('webhook', 's3')),
  config             jsonb       not null check (jsonb_typeof(config) = 'object'),
  secret_enc         text        not null check (length(secret_enc) between 1 and 4000),
  events             text[],
  enabled            boolean     not null default true,
  cursor_inserted_at timestamptz,
  cursor_uuid        uuid,
  status             text        not null default 'ok' check (status in ('ok', 'retrying', 'failing')),
  attempts           integer     not null default 0,
  next_attempt_at    timestamptz,
  last_error         text,
  last_success_at    timestamptz,
  delivered          bigint      not null default 0,
  created_at         timestamptz not null default now(),
  updated_at         timestamptz not null default now(),
  unique (project_id, name)
);
create index if not exists export_destinations_due_idx on pa.export_destinations (enabled, next_attempt_at);

create table if not exists pa.export_runs (
  id             uuid        primary key default gen_random_uuid(),
  project_id     uuid        not null,
  destination_id uuid        not null references pa.export_destinations (id) on delete cascade,
  started_at     timestamptz not null,
  finished_at    timestamptz not null,
  attempt        integer     not null,
  rows           integer     not null default 0,
  bytes          integer     not null default 0,
  status         text        not null check (status in ('ok', 'failed')),
  http_status    integer,
  error          text
);
create index if not exists export_runs_destination_idx on pa.export_runs (project_id, destination_id, started_at desc);
