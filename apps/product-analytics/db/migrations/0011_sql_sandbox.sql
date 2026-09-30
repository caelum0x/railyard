-- SQL over events (A2). Customers query four views in schema pa_view:
-- events, persons, groups and sessions. The views read only the project
-- registered for the calling backend in pa.sql_sandbox_sessions, a table the
-- sandbox role cannot read or write; so a query that slips past the parser
-- (set_config, other schemas, several statements) still sees one project.
--
-- The sandbox connects as pa_sql_login (a login role that is a member of
-- nothing but pa_sql), never as the owner, so `set role` from inside a query
-- cannot switch to a privileged role. The service sets its password at start
-- (src/insights/sandbox.ts); the migration never stores one.

create schema if not exists pa_view;

do $$
begin
  if not exists (select 1 from pg_roles where rolname = 'pa_sql') then
    create role pa_sql nologin;
  end if;
  if not exists (select 1 from pg_roles where rolname = 'pa_sql_login') then
    create role pa_sql_login nologin noinherit;
  end if;
end
$$;

grant pa_sql to pa_sql_login;
alter role pa_sql_login inherit;
alter role pa_sql_login set statement_timeout = '10s';
alter role pa_sql_login set default_transaction_read_only = on;
alter role pa_sql_login set idle_in_transaction_session_timeout = '15s';

create table if not exists pa.sql_sandbox_sessions (
  pid        integer     primary key,
  project_id uuid        not null,
  started_at timestamptz not null default now()
);
revoke all on pa.sql_sandbox_sessions from public;
-- Row level security on with no policy: no request role sees a row; only the
-- owner (the service's sandbox runner and the security-definer function) does.
alter table pa.sql_sandbox_sessions enable row level security;

-- The project of the calling backend, or null (every view is then empty).
create or replace function pa_view.project() returns uuid
language sql stable security definer
set search_path = pg_catalog, pa
as $$ select project_id from pa.sql_sandbox_sessions where pid = pg_backend_pid() $$;

revoke all on function pa_view.project() from public;
grant usage on schema pa_view to pa_sql;
grant execute on function pa_view.project() to pa_sql;

create or replace view pa_view.events with (security_barrier) as
select e.uuid,
       e.event,
       e.distinct_id,
       coalesce(o.new_person_id, e.person_id) as person_id,
       e."timestamp",
       e.properties,
       e.elements_chain,
       e.session_id,
       e.window_id,
       e."$group_0" as group_0,
       e."$group_1" as group_1,
       e."$group_2" as group_2,
       e."$group_3" as group_3,
       e."$group_4" as group_4
  from pa.events e
  left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
 where e.project_id = pa_view.project();

create or replace view pa_view.persons with (security_barrier) as
select p.id,
       p.properties,
       p.is_identified,
       p.created_at,
       (select array_agg(d.distinct_id order by d.distinct_id)
          from pa.person_distinct_ids d
         where d.project_id = p.project_id and d.person_id = p.id) as distinct_ids
  from pa.persons p
 where p.project_id = pa_view.project();

create or replace view pa_view.groups with (security_barrier) as
select t.group_type,
       g.group_type_index,
       g.group_key,
       g.properties,
       g.created_at,
       g.updated_at
  from pa.groups g
  join pa.group_types t on t.project_id = g.project_id and t.group_type_index = g.group_type_index
 where g.project_id = pa_view.project();

create or replace view pa_view.sessions with (security_barrier) as
select e.session_id,
       (array_agg(coalesce(o.new_person_id, e.person_id) order by e."timestamp"))[1] as person_id,
       min(e."timestamp") as started_at,
       max(e."timestamp") as ended_at,
       extract(epoch from max(e."timestamp") - min(e."timestamp"))::bigint as duration_seconds,
       count(*) as events,
       count(*) filter (where e.event in ('$pageview', '$screen')) as views,
       (array_agg(coalesce(e.properties ->> '$current_url', e.properties ->> '$screen_name') order by e."timestamp")
          filter (where e.event in ('$pageview', '$screen')))[1] as entry,
       (array_agg(coalesce(e.properties ->> '$current_url', e.properties ->> '$screen_name') order by e."timestamp" desc)
          filter (where e.event in ('$pageview', '$screen')))[1] as exit
  from pa.events e
  left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id
 where e.project_id = pa_view.project() and e.session_id is not null
 group by e.session_id;

grant select on pa_view.events, pa_view.persons, pa_view.groups, pa_view.sessions to pa_sql;

-- The sandbox role reaches nothing in schema pa.
revoke all on schema pa from pa_sql, pa_sql_login;
