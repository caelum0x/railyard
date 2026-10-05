-- Alerts on saved insights (QY1.4, A25): a threshold (above or below a value)
-- or an anomaly (the latest complete interval this many standard deviations
-- from the ones before it). Checked by the scheduler; an alert mails its
-- recipients when it starts firing, once, until it clears. Idempotent.

create table if not exists pa.alerts (
  id              uuid        primary key default gen_random_uuid(),
  project_id      uuid        not null,
  insight_id      uuid        not null references pa.saved_insights (id) on delete cascade,
  name            text        not null check (length(name) between 1 and 200),
  kind            text        not null check (kind in ('threshold', 'anomaly')),
  series_index    smallint    not null default 0 check (series_index between 0 and 19),
  direction       text        not null check (direction in ('above', 'below', 'either')),
  threshold       double precision,
  sensitivity     double precision check (sensitivity is null or sensitivity between 1 and 10),
  check_every     text        not null default 'hour' check (check_every in ('hour', 'day')),
  recipients      text[]      not null check (cardinality(recipients) between 1 and 20),
  enabled         boolean     not null default true,
  state           text        not null default 'ok' check (state in ('ok', 'firing', 'error')),
  last_value      double precision,
  last_checked_at timestamptz,
  last_fired_at   timestamptz,
  last_error      text,
  next_check_at   timestamptz,
  created_at      timestamptz not null default now(),
  updated_at      timestamptz not null default now(),
  check ((kind = 'threshold' and threshold is not null and direction <> 'either') or (kind = 'anomaly' and sensitivity is not null)),
  unique (project_id, name)
);
create index if not exists alerts_due_idx on pa.alerts (enabled, next_check_at);
create index if not exists alerts_insight_idx on pa.alerts (project_id, insight_id);
