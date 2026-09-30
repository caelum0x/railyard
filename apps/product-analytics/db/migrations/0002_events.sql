-- Events: range-partitioned by month on `timestamp`, person_id on every row.
-- Idempotency lives in pa.ingest_receipts (a unique key on a partitioned
-- table must contain the partition key, so the uuid alone cannot be unique here).

create table if not exists pa.events (
  uuid           uuid        not null,
  project_id     uuid        not null,
  event          text        not null check (length(event) between 1 and 200),
  distinct_id    text        not null check (length(distinct_id) between 1 and 200),
  person_id      uuid        not null,
  "timestamp"    timestamptz not null,
  properties     jsonb       not null default '{}'::jsonb,
  elements_chain text,
  session_id     text,
  window_id      text,
  "$group_0"     text,
  "$group_1"     text,
  "$group_2"     text,
  "$group_3"     text,
  "$group_4"     text,
  inserted_at    timestamptz not null default now()
) partition by range ("timestamp");

create index if not exists events_project_event_ts_idx on pa.events (project_id, event, "timestamp");
create index if not exists events_project_person_ts_idx on pa.events (project_id, person_id, "timestamp");
create index if not exists events_project_session_idx on pa.events (project_id, session_id);
create index if not exists events_project_distinct_idx on pa.events (project_id, distinct_id);
create index if not exists events_project_uuid_idx on pa.events (project_id, uuid);

-- The partition name of a month: events_yYYYYmMM.
create or replace function pa.partition_name(month_start date) returns text
language sql immutable
as $$ select 'events_y' || to_char(month_start, 'YYYY') || 'm' || to_char(month_start, 'MM') $$;

-- Creates the partition holding `day`'s month when it is missing. Returns its name.
create or replace function pa.ensure_month(day date) returns text
language plpgsql
as $$
declare
  m date := date_trunc('month', day)::date;
  n text := pa.partition_name(m);
begin
  if to_regclass('pa.' || n) is null then
    execute format(
      'create table if not exists pa.%I partition of pa.events for values from (%L) to (%L)',
      n, to_char(m, 'YYYY-MM-DD') || ' 00:00:00+00', to_char((m + interval '1 month')::date, 'YYYY-MM-DD') || ' 00:00:00+00'
    );
  end if;
  return n;
end
$$;

-- Drops whole monthly partitions that end on or before `cutoff`. Returns the names dropped.
create or replace function pa.drop_months_before(cutoff timestamptz) returns setof text
language plpgsql
as $$
declare
  r record;
  upper_bound timestamptz;
begin
  for r in
    select c.relname, pg_get_expr(c.relpartbound, c.oid) as bound
    from pg_inherits i
    join pg_class c on c.oid = i.inhrelid
    join pg_class p on p.oid = i.inhparent
    join pg_namespace ns on ns.oid = p.relnamespace
    where ns.nspname = 'pa' and p.relname = 'events'
  loop
    upper_bound := substring(r.bound from 'TO \(''([^'']+)''\)')::timestamptz;
    if upper_bound is not null and upper_bound <= cutoff then
      execute format('drop table pa.%I', r.relname);
      return next r.relname;
    end if;
  end loop;
end
$$;
