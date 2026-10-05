// QY1.4 migrations (0025-0029): idempotent; the catalogue kinds are allowed;
// alerts, subscriptions, the mail log and unsubscribes are under row level
// security, so the request role sees and writes only its own project's rows.
import { test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { migrate, MIGRATIONS } from "../../scripts/migrate.ts";
import { migratedPg, skipNoPg } from "../a1/helpers.ts";

const TABLES = ["alerts", "subscriptions", "mail_log", "mail_unsubscribes"];

test("0025-0029 apply twice; every new table is under row level security; the catalogue kinds are allowed", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  for (const m of ["0025", "0026", "0027", "0028", "0029"]) assert.ok(MIGRATIONS.some((x) => x.startsWith(m)), m);
  await migrate(pg.url);
  assert.equal(pg.psql("select count(*) from pg_class c join pg_namespace n on n.oid = c.relnamespace where n.nspname = 'pa' and c.relkind in ('r','p') and not c.relispartition and not c.relrowsecurity"), "0");
  const p = randomUUID();
  for (const kind of ["webhook", "s3", "slack", "discord", "hubspot"]) pg.psql(`insert into pa.export_destinations (project_id, name, kind, config, secret_enc) values ('${p}', '${kind}', '${kind}', '{}', 'x')`);
  assert.throws(() => pg.psql(`insert into pa.export_destinations (project_id, name, kind, config, secret_enc) values ('${p}', 'ftp', 'ftp', '{}', 'x')`), /check constraint/);
});

test("the request role sees and writes only its own project's alerts, subscriptions and mail rows", { skip: skipNoPg }, async () => {
  const pg = await migratedPg();
  const a = randomUUID();
  const b = randomUUID();
  for (const p of [a, b]) {
    const i = pg.psql(`insert into pa.saved_insights (project_id, name, kind, query) values ('${p}', 'i', 'trends', '{}') returning id`);
    const d = pg.psql(`insert into pa.dashboards (project_id, name) values ('${p}', 'd') returning id`);
    const al = pg.psql(`insert into pa.alerts (project_id, insight_id, name, kind, direction, threshold, recipients) values ('${p}', '${i}', 'a', 'threshold', 'above', 1, array['x@y.example']) returning id`);
    pg.psql(`insert into pa.subscriptions (project_id, dashboard_id, weekday, hour_utc, recipients, next_send_at) values ('${p}', '${d}', 1, 8, array['x@y.example'], now())`);
    pg.psql(`insert into pa.mail_log (project_id, source, source_id, recipient, status) values ('${p}', 'alert', '${al}', 'x@y.example', 'sent')`);
    pg.psql(`insert into pa.mail_unsubscribes (project_id, email) values ('${p}', 'x@y.example')`);
  }
  const asA = (sql: string) => pg.session("begin", "set local role pa_app", `select set_config('pa.project_id', '${a}', true)`, sql, "commit");
  for (const t of TABLES) assert.match(asA(`select count(*) from pa.${t}`), /^1$/m, t);
  assert.throws(() => asA(`insert into pa.mail_unsubscribes (project_id, email) values ('${b}', 'z@y.example')`), /row-level security/);
  assert.doesNotMatch(asA(`update pa.alerts set enabled = false where project_id = '${b}' returning 1`), /^1$/m);
  // Shape checks: a threshold alert needs its number; an anomaly its sensitivity; at most 20 recipients.
  const i = pg.psql(`select id from pa.saved_insights where project_id = '${a}'`);
  assert.throws(() => pg.psql(`insert into pa.alerts (project_id, insight_id, name, kind, direction, recipients) values ('${a}', '${i}', 'n', 'threshold', 'above', array['x@y.example'])`), /check constraint/);
  assert.throws(() => pg.psql(`insert into pa.alerts (project_id, insight_id, name, kind, direction, recipients) values ('${a}', '${i}', 'm', 'anomaly', 'either', array['x@y.example'])`), /check constraint/);
  assert.throws(() => pg.psql(`insert into pa.alerts (project_id, insight_id, name, kind, direction, threshold, recipients) values ('${a}', '${i}', 'r', 'threshold', 'above', 1, array[]::text[])`), /check constraint/);
});
