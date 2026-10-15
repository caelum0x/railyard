-- Lookup tables and data views (Mixpanel parity checklist rows 4 and 5):
--   lookup tables   CSV-backed key -> columns, joined into event/person/group
--                    properties at query time by a key the query names.
--   data views      a named, reusable set of property filters, applied to an
--                    Insights/Funnels/etc. query by id (query.data_view_id).
-- Idempotent. Same shape and RLS policy as the other insights tables (0010,
-- 0012): every row carries project_id; the request role sees only
-- pa.current_project().

create table if not exists pa.lookup_tables (
  id          uuid        primary key default gen_random_uuid(),
  project_id  uuid        not null,
  name        text        not null check (length(name) between 1 and 200),
  key_column  text        not null check (length(key_column) between 1 and 200),
  columns     jsonb       not null check (jsonb_typeof(columns) = 'array'),
  row_count   integer     not null default 0,
  created_at  timestamptz not null default now(),
  updated_at  timestamptz not null default now(),
  unique (project_id, name)
);

-- One row per key; `data` holds every non-key column, so a query reads
-- `data ->> 'column_name'` the same way event/person properties already do.
create table if not exists pa.lookup_table_rows (
  project_id       uuid  not null,
  lookup_table_id  uuid  not null references pa.lookup_tables (id) on delete cascade,
  key              text  not null check (length(key) between 1 and 400),
  data             jsonb not null default '{}'::jsonb,
  primary key (project_id, lookup_table_id, key)
);

create table if not exists pa.data_views (
  id          uuid        primary key default gen_random_uuid(),
  project_id  uuid        not null,
  name        text        not null check (length(name) between 1 and 200),
  filters     jsonb       not null check (jsonb_typeof(filters) = 'array'),
  created_at  timestamptz not null default now(),
  updated_at  timestamptz not null default now(),
  unique (project_id, name)
);

do $$
declare
  t text;
begin
  foreach t in array array['lookup_tables', 'lookup_table_rows', 'data_views']
  loop
    execute format('alter table pa.%I enable row level security', t);
    execute format('drop policy if exists project_isolation on pa.%I', t);
    execute format(
      'create policy project_isolation on pa.%I for all to pa_app using (project_id = pa.current_project()) with check (project_id = pa.current_project())',
      t
    );
    execute format('grant select, insert, update, delete on pa.%I to pa_app', t);
  end loop;
end
$$;

do $$
begin
  if exists (select 1 from pg_roles where rolname = 'anon') then
    execute 'revoke all on pa.lookup_tables, pa.lookup_table_rows, pa.data_views from anon';
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    execute 'revoke all on pa.lookup_tables, pa.lookup_table_rows, pa.data_views from authenticated';
  end if;
end
$$;
