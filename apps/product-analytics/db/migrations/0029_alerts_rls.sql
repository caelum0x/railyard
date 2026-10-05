-- Row level security for the QY1.4 tables (same policy as 0024): the request
-- role sees only pa.current_project(), and the service also binds project_id.

do $$
declare
  t text;
begin
  foreach t in array array['alerts', 'subscriptions', 'mail_log', 'mail_unsubscribes']
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
    execute 'revoke all on all tables in schema pa from anon';
  end if;
  if exists (select 1 from pg_roles where rolname = 'authenticated') then
    execute 'revoke all on all tables in schema pa from authenticated';
  end if;
end
$$;
