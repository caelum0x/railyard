// A3's migrations (0020-0024): idempotent; every A3 table is under row level
// security; the request role sees only its own project's replays, surveys,
// experiments and export destinations and cannot write another project's rows.
import { test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { migrate, MIGRATIONS } from "../../scripts/migrate.ts";
import { migratedPg, skipNoPg } from "../a1/helpers.ts";

const A3_TABLES = ["recording_settings", "replays", "replay_chunks", "surveys", "experiments", "export_destinations", "export_runs"];

test("A3 migrations apply twice and keep every table under row level security", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  for (const m of ["0020", "0021", "0022", "0023", "0024"]) assert.ok(MIGRATIONS.some((x) => x.startsWith(m)), m);
  await migrate(pg.url);
  const tables = pg.psql("select string_agg(c.relname, ',') from pg_class c join pg_namespace n on n.oid = c.relnamespace where n.nspname = 'pa' and c.relkind = 'r' and not c.relispartition").split(",");
  for (const t of A3_TABLES) assert.ok(tables.includes(t), t);
  assert.equal(pg.psql("select count(*) from pg_class c join pg_namespace n on n.oid = c.relnamespace where n.nspname = 'pa' and c.relkind in ('r','p') and not c.relispartition and not c.relrowsecurity"), "0");
  assert.equal(pg.psql("select count(*) from information_schema.role_table_grants where table_schema = 'pa' and grantee in ('anon', 'authenticated')"), "0");
});

test("the request role sees and writes only its own project's A3 rows", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  const a = randomUUID();
  const b = randomUUID();
  const org = randomUUID();
  for (const p of [a, b]) {
    pg.psql(`insert into pa.recording_settings (project_id, org_id) values ('${p}', '${org}')`);
    pg.psql(`insert into pa.replays (project_id, session_id, distinct_id, person_id, started_at, ended_at, expires_at) values ('${p}', 's1', 'u1', '${randomUUID()}', now(), now(), now() + interval '30 days')`);
    pg.psql(`insert into pa.replay_chunks (project_id, session_id, seq, object_key, first_ts, last_ts, events, bytes, raw_bytes) values ('${p}', 's1', 1, 'k', now(), now(), 1, 1, 1)`);
    pg.psql(`insert into pa.surveys (project_id, name, questions) values ('${p}', 'nps', '[]')`);
    pg.psql(`insert into pa.experiments (project_id, name, flag_key, flags_env, variants, metric) values ('${p}', 'x', 'new_checkout', 'production', '[]', '{}')`);
    const d = pg.psql(`insert into pa.export_destinations (project_id, name, kind, config, secret_enc) values ('${p}', 'hook', 'webhook', '{}', 'x') returning id`);
    pg.psql(`insert into pa.export_runs (project_id, destination_id, started_at, finished_at, attempt, status) values ('${p}', '${d}', now(), now(), 1, 'ok')`);
  }
  const asA = (sql: string) => pg.session("begin", "set local role pa_app", `select set_config('pa.project_id', '${a}', true)`, sql, "commit");
  for (const t of A3_TABLES) assert.match(asA(`select count(*) from pa.${t}`), /^1$/m, t);
  assert.throws(() => asA(`insert into pa.surveys (project_id, name, questions) values ('${b}', 'other', '[]')`), /row-level security/);
  assert.doesNotMatch(asA(`update pa.replays set distinct_id = 'x' where project_id = '${b}' returning 1`), /^1$/m);
  assert.equal(pg.psql(`select distinct_id from pa.replays where project_id = '${b}'`), "u1");
  assert.throws(() => asA(`update pa.replays set project_id = '${b}' where project_id = '${a}'`), /row-level security/);
  assert.doesNotMatch(asA(`delete from pa.experiments where project_id = '${b}' returning 1`), /^1$/m);
  assert.equal(pg.psql(`select count(*) from pa.experiments where project_id = '${b}'`), "1");
});

test("one running experiment per flag, and surveys and experiments keep valid names and keys", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  const p = randomUUID();
  pg.psql(`insert into pa.experiments (project_id, name, flag_key, flags_env, variants, metric, status) values ('${p}', 'one', 'checkout', 'production', '[]', '{}', 'running')`);
  assert.throws(() => pg.psql(`insert into pa.experiments (project_id, name, flag_key, flags_env, variants, metric, status) values ('${p}', 'two', 'checkout', 'production', '[]', '{}', 'running')`), /duplicate key/);
  pg.psql(`insert into pa.experiments (project_id, name, flag_key, flags_env, variants, metric, status) values ('${p}', 'three', 'checkout', 'production', '[]', '{}', 'stopped')`);
  assert.throws(() => pg.psql(`insert into pa.experiments (project_id, name, flag_key, flags_env, variants, metric) values ('${p}', 'bad', 'Bad Key', 'production', '[]', '{}')`), /check constraint/);
  assert.throws(() => pg.psql(`insert into pa.surveys (project_id, name, questions, url_match) values ('${p}', 's', '[]', 'glob')`), /check constraint/);
});
