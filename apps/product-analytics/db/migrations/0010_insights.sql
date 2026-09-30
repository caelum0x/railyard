-- Insights (A2): actions, cohorts, saved insights, dashboards, the result
-- cache and the rollup watermark. Idempotent. Every table carries project_id
-- and is scoped by row level security for the request role (0012).

create table if not exists pa.actions (
  id          uuid        primary key default gen_random_uuid(),
  project_id  uuid        not null,
  name        text        not null check (length(name) between 1 and 200),
  steps       jsonb       not null check (jsonb_typeof(steps) = 'array'),
  created_at  timestamptz not null default now(),
  updated_at  timestamptz not null default now(),
  unique (project_id, name)
);

create table if not exists pa.cohorts (
  id           uuid        primary key default gen_random_uuid(),
  project_id   uuid        not null,
  name         text        not null check (length(name) between 1 and 200),
  kind         text        not null check (kind in ('static', 'behavioural')),
  definition   jsonb       not null default '{}'::jsonb,
  version      integer     not null default 0,
  people_count bigint      not null default 0,
  computed_at  timestamptz,
  error        text,
  created_at   timestamptz not null default now(),
  unique (project_id, name)
);

-- Members of a cohort as person ids. Queries resolve them through
-- pa.person_overrides, so a later merge keeps the member.
create table if not exists pa.cohort_people (
  project_id uuid    not null,
  cohort_id  uuid    not null references pa.cohorts (id) on delete cascade,
  person_id  uuid    not null,
  primary key (project_id, cohort_id, person_id)
);
create index if not exists cohort_people_person_idx on pa.cohort_people (project_id, person_id);

create table if not exists pa.saved_insights (
  id          uuid        primary key default gen_random_uuid(),
  project_id  uuid        not null,
  name        text        not null check (length(name) between 1 and 200),
  kind        text        not null check (kind in ('trends', 'funnels', 'retention', 'paths', 'stickiness', 'lifecycle')),
  query       jsonb       not null,
  created_at  timestamptz not null default now(),
  updated_at  timestamptz not null default now()
);
create index if not exists saved_insights_project_idx on pa.saved_insights (project_id, updated_at desc);

create table if not exists pa.dashboards (
  id         uuid        primary key default gen_random_uuid(),
  project_id uuid        not null,
  name       text        not null check (length(name) between 1 and 200),
  created_at timestamptz not null default now(),
  unique (project_id, name)
);

create table if not exists pa.dashboard_tiles (
  id           uuid     primary key default gen_random_uuid(),
  project_id   uuid     not null,
  dashboard_id uuid     not null references pa.dashboards (id) on delete cascade,
  insight_id   uuid     not null references pa.saved_insights (id) on delete cascade,
  position     smallint not null default 0 check (position between 0 and 99),
  unique (dashboard_id, insight_id)
);
create index if not exists dashboard_tiles_dashboard_idx on pa.dashboard_tiles (project_id, dashboard_id, position);

-- Results by (normalized query hash) with the data version they were
-- computed at. A cached result with an older data version is shown as stale
-- with its computed-at time until it is refreshed.
create table if not exists pa.insight_cache (
  project_id   uuid        not null,
  cache_key    text        not null check (length(cache_key) = 64),
  result       jsonb       not null,
  data_version text        not null,
  computed_at  timestamptz not null,
  took_ms      integer     not null default 0,
  primary key (project_id, cache_key)
);
create index if not exists insight_cache_computed_idx on pa.insight_cache (computed_at);

-- Hourly rollups (pa.rollup_hourly, 0006) are complete for every hour of
-- events inserted before rolled_to. reconcile_after marks a project whose
-- rollups must be rebuilt (a person was deleted).
create table if not exists pa.rollup_state (
  project_id      uuid        primary key,
  rolled_to       timestamptz not null,
  reconcile_after timestamptz
);

-- Bumped by writes that change answers without new events (cohort
-- recompute, a person deleted), so cached results become stale.
create table if not exists pa.insight_versions (
  project_id uuid   primary key,
  version    bigint not null default 0
);
