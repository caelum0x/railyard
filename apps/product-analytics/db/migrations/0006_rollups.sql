-- Hourly event counts per project and event name. Written by the rollup job (A2).

create table if not exists pa.rollup_hourly (
  project_id uuid        not null,
  event      text        not null,
  hour       timestamptz not null check (date_trunc('hour', hour) = hour),
  count      bigint      not null default 0 check (count >= 0),
  primary key (project_id, event, hour)
);
