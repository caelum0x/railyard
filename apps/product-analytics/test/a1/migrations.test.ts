// Migrations 0001-0009 (and every later lane's) apply twice on a scratch cluster; RLS is on for every
// table; the request role can never read or write another project's rows
// (the SQL-level half of the isolation acceptance); anon has no access.
import { test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { migrate, MIGRATIONS } from "../../scripts/migrate.ts";
import { migratedPg, skipNoPg } from "./helpers.ts";

test("migrations apply twice, every pa table has RLS, anon has nothing", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  // A1's migrations come first; later lanes (A2 0010+, A3 0020+) add theirs after.
  assert.deepEqual(MIGRATIONS.slice(0, 9).map((m) => m.slice(0, 4)), ["0001", "0002", "0003", "0004", "0005", "0006", "0007", "0008", "0009"]);
  await migrate(pg.url);
  const tables = pg.psql("select string_agg(c.relname, ',' order by c.relname) from pg_class c join pg_namespace n on n.oid = c.relnamespace where n.nspname = 'pa' and c.relkind in ('r','p') and not c.relispartition");
  const a1Tables = ["events", "gdpr_jobs", "group_types", "groups", "ingest_receipts", "person_distinct_ids", "person_overrides", "persons", "project_firsts", "project_settings", "rollup_hourly", "usage_monthly"];
  for (const t of a1Tables) assert.ok(tables.split(",").includes(t), t);
  assert.equal(pg.psql("select count(*) from pg_class c join pg_namespace n on n.oid = c.relnamespace where n.nspname = 'pa' and c.relkind in ('r','p') and not c.relispartition and not c.relrowsecurity"), "0");
  assert.equal(pg.psql("select count(*) from information_schema.role_table_grants where table_schema = 'pa' and grantee in ('anon', 'authenticated')"), "0");
  assert.equal(pg.psql("select rolbypassrls::text from pg_roles where rolname = 'pa_app'"), "false");
});

test("monthly partitions: ensure_month is idempotent; drop_months_before drops only whole months before the cutoff", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  assert.equal(pg.psql("select pa.ensure_month('2026-09-17')"), "events_y2026m09");
  assert.equal(pg.psql("select pa.ensure_month('2026-09-30')"), "events_y2026m09");
  pg.psql("select pa.ensure_month('2026-08-01'); select pa.ensure_month('2026-10-01')");
  assert.equal(pg.psql("select string_agg(n, ',' order by n) from pa.drop_months_before('2026-09-15T00:00:00Z') n"), "events_y2026m08");
  assert.equal(pg.psql("select count(*) from pg_inherits i join pg_class p on p.oid = i.inhparent where p.relname = 'events'"), "2");
});

test("the request role sees and writes only the project set for the transaction (RLS)", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  const a = randomUUID();
  const b = randomUUID();
  pg.psql("select pa.ensure_month(now()::date)");
  for (const p of [a, b]) {
    pg.psql(`insert into pa.events (uuid, project_id, event, distinct_id, person_id, "timestamp") values (gen_random_uuid(), '${p}', 'e', 'd', gen_random_uuid(), now())`);
    pg.psql(`insert into pa.persons (project_id) values ('${p}')`);
  }
  const asA = (sql: string) => pg.session("begin", "set local role pa_app", `select set_config('pa.project_id', '${a}', true)`, sql, "commit");
  assert.match(asA("select count(*) from pa.events"), /1$/);
  assert.match(asA(`select count(*) from pa.events where project_id = '${b}'`), /0$/);
  assert.match(asA("select count(*) from pa.persons"), /1$/);
  assert.throws(() => asA(`insert into pa.events (uuid, project_id, event, distinct_id, person_id, "timestamp") values (gen_random_uuid(), '${b}', 'x', 'd', gen_random_uuid(), now())`), /row-level security/);
  assert.throws(() => asA(`insert into pa.persons (project_id) values ('${b}')`), /row-level security/);
  asA(`update pa.persons set is_identified = true where project_id = '${b}'`);
  asA(`delete from pa.events where project_id = '${b}'`);
  assert.equal(pg.psql(`select count(*) from pa.events where project_id = '${b}'`), "1");
  assert.equal(pg.psql(`select count(*) from pa.persons where project_id = '${b}' and is_identified`), "0");
  // No project set: nothing is visible at all.
  assert.match(pg.session("begin", "set local role pa_app", "select count(*) from pa.events", "commit"), /0$/);
  // Partitions are not reachable directly by the request role.
  const part = pg.psql("select pa.partition_name(date_trunc('month', now())::date)");
  assert.throws(() => pg.session("begin", "set local role pa_app", `select count(*) from pa.${part}`, "commit"), /permission denied/);
});
