-- Session replay (A3): per-project recording switches, the replay index and
-- its chunks. The recorded bytes live in S3-compatible object storage
-- (gzip NDJSON, one rrweb event per line); Postgres holds only the index.
-- Idempotent. Row level security in 0024.

create table if not exists pa.recording_settings (
  project_id             uuid         primary key,
  org_id                 uuid         not null,
  replay_enabled         boolean      not null default false,
  replay_sample_rate     numeric(4,3) not null default 0.1 check (replay_sample_rate between 0 and 1),
  replay_min_duration_ms integer      not null default 0 check (replay_min_duration_ms between 0 and 30000),
  heatmaps_enabled       boolean      not null default false,
  updated_at             timestamptz  not null default now()
);

create table if not exists pa.replays (
  project_id     uuid        not null,
  session_id     text        not null check (length(session_id) between 1 and 200),
  distinct_id    text        not null check (length(distinct_id) between 1 and 200),
  person_id      uuid        not null,
  started_at     timestamptz not null,
  ended_at       timestamptz not null,
  click_count    integer     not null default 0,
  keypress_count integer     not null default 0,
  event_count    integer     not null default 0,
  urls           text[]      not null default '{}',
  size_bytes     bigint      not null default 0,
  raw_bytes      bigint      not null default 0,
  chunk_count    integer     not null default 0,
  dropped_chunks integer     not null default 0,
  expires_at     timestamptz not null,
  created_at     timestamptz not null default now(),
  primary key (project_id, session_id),
  check (ended_at >= started_at)
);
create index if not exists replays_project_started_idx on pa.replays (project_id, started_at desc);
create index if not exists replays_project_distinct_idx on pa.replays (project_id, distinct_id);
create index if not exists replays_project_person_idx on pa.replays (project_id, person_id);
create index if not exists replays_expires_idx on pa.replays (expires_at);

create table if not exists pa.replay_chunks (
  project_id uuid        not null,
  session_id text        not null,
  seq        integer     not null check (seq >= 1),
  object_key text        not null check (length(object_key) between 1 and 1024),
  window_ids text[]      not null default '{}',
  first_ts   timestamptz not null,
  last_ts    timestamptz not null,
  events     integer     not null check (events >= 0),
  bytes      integer     not null check (bytes >= 0),
  raw_bytes  integer     not null check (raw_bytes >= 0),
  created_at timestamptz not null default now(),
  primary key (project_id, session_id, seq),
  foreign key (project_id, session_id) references pa.replays (project_id, session_id) on delete cascade
);
