-- Product analytics (A1): schema, the request role and the project setting.
-- Idempotent: every statement can run twice. Run by the database owner.
--
-- Request paths run as `pa_app` (no BYPASSRLS) with `pa.project_id` set per
-- transaction; background jobs run as the owner and always bind project_id.

create schema if not exists pa;

do $$
begin
  if not exists (select 1 from pg_roles where rolname = 'pa_app') then
    create role pa_app nologin;
  end if;
end
$$;

grant usage on schema pa to pa_app;

-- The project of the current transaction, or null when none is set.
create or replace function pa.current_project() returns uuid
language sql stable
as $$ select nullif(current_setting('pa.project_id', true), '')::uuid $$;

grant execute on function pa.current_project() to pa_app;
