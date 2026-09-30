// A2's migrations (0010-0014): idempotent; every pa table keeps row level
// security; anon and authenticated get nothing; the request role sees only
// its project in the insights tables; the SQL roles cannot bypass row level
// security, are not members of the request role, and reach only pa_view.
import { test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { migrate, MIGRATIONS } from "../../scripts/migrate.ts";
import { migratedPg, skipNoPg } from "../a1/helpers.ts";

test("A2 migrations apply twice and keep every table under row level security", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  for (const m of ["0010", "0011", "0012", "0013", "0014"]) assert.ok(MIGRATIONS.some((x) => x.startsWith(m)), m);
  await migrate(pg.url);
  const tables = pg.psql("select string_agg(c.relname, ',' order by c.relname) from pg_class c join pg_namespace n on n.oid = c.relnamespace where n.nspname = 'pa' and c.relkind in ('r','p') and not c.relispartition");
  for (const t of ["actions", "cohorts", "cohort_people", "saved_insights", "dashboards", "dashboard_tiles", "insight_cache", "rollup_state", "insight_versions", "sql_sandbox_sessions"]) assert.ok(tables.split(",").includes(t), t);
  assert.equal(pg.psql("select count(*) from pg_class c join pg_namespace n on n.oid = c.relnamespace where n.nspname = 'pa' and c.relkind in ('r','p') and not c.relispartition and not c.relrowsecurity"), "0");
  assert.equal(pg.psql("select count(*) from information_schema.role_table_grants where table_schema in ('pa', 'pa_view') and grantee in ('anon', 'authenticated')"), "0");
  assert.equal(pg.psql("select string_agg(rolname || ':' || rolbypassrls::text || ':' || rolsuper::text, ',' order by rolname) from pg_roles where rolname in ('pa_sql', 'pa_sql_login')"), "pa_sql:false:false,pa_sql_login:false:false");
  assert.equal(pg.psql("select pg_has_role('pa_sql_login', 'pa_app', 'member')::text"), "false");
  assert.equal(pg.psql("select has_schema_privilege('pa_sql_login', 'pa', 'usage')::text"), "false");
  assert.equal(pg.psql("select has_table_privilege('pa_sql_login', 'pa_view.events', 'select')::text"), "true");
  assert.equal(pg.psql("select count(*) from pg_indexes where indexname in ('events_project_inserted_idx', 'events_project_event_ts_person_idx')"), "2");
});

test("the request role sees only its project's saved insights, segments and cache", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  const a = randomUUID();
  const b = randomUUID();
  for (const p of [a, b]) {
    pg.psql(`insert into pa.saved_insights (project_id, name, kind, query) values ('${p}', 'x', 'trends', '{}')`);
    pg.psql(`insert into pa.cohorts (project_id, name, kind) values ('${p}', 'x', 'static')`);
    pg.psql(`insert into pa.insight_cache (project_id, cache_key, result, data_version, computed_at) values ('${p}', repeat('a', 64), '{}', '1', now())`);
  }
  const asA = (sql: string) => pg.session("begin", "set local role pa_app", `select set_config('pa.project_id', '${a}', true)`, sql, "commit");
  for (const t of ["saved_insights", "cohorts", "insight_cache"]) assert.match(asA(`select count(*) from pa.${t}`), /1$/, t);
  assert.throws(() => asA("select count(*) from pa.sql_sandbox_sessions"), /permission denied/);
  assert.throws(() => asA(`insert into pa.saved_insights (project_id, name, kind, query) values ('${b}', 'y', 'trends', '{}')`), /row-level security/);
});
