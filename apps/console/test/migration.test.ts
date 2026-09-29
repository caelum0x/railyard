// The console migration applies to a scratch Postgres twice without error
// (idempotent) and leaves RLS on with no grants for anon. Never a hosted DB.
import { test, after } from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const MIGRATION = join(ROOT, "db/migrations/0001_railyard_console.sql");
const have = spawnSync("pg_ctl", ["--version"]).status === 0 && spawnSync("initdb", ["--version"]).status === 0;

test("0001 applies twice on a scratch cluster; RLS on; anon has no access", { skip: have ? false : "pg_ctl/initdb not installed" }, () => {
  const dir = mkdtempSync(join(tmpdir(), "ry-pg-"));
  const port = String(55000 + Math.floor(Math.random() * 2000));
  after(() => {
    spawnSync("pg_ctl", ["-D", join(dir, "data"), "-m", "immediate", "stop"], { stdio: "ignore" });
    rmSync(dir, { recursive: true, force: true });
  });
  execFileSync("initdb", ["-D", join(dir, "data"), "-U", "postgres", "-A", "trust", "--no-sync"], { stdio: "ignore" });
  execFileSync("pg_ctl", ["-D", join(dir, "data"), "-o", `-p ${port} -k ${dir} -c listen_addresses=''`, "-w", "start"], { stdio: "ignore" });
  const psql = (args: string[]) => execFileSync("psql", ["-h", dir, "-p", port, "-U", "postgres", "-v", "ON_ERROR_STOP=1", "-qAt", ...args], { encoding: "utf8" });
  psql(["-c", "create role anon nologin; create role authenticated nologin; create role service_role nologin bypassrls;"]);
  psql(["-f", MIGRATION]);
  psql(["-f", MIGRATION]);
  const tables = psql(["-c", "select string_agg(tablename, ',' order by tablename) from pg_tables where schemaname = 'railyard'"]).trim();
  assert.equal(tables, "api_tokens,apps,billing_events,deploys,device_codes,entitlements,org_members,orgs");
  const noRls = psql(["-c", "select count(*) from pg_tables where schemaname = 'railyard' and not rowsecurity"]).trim();
  assert.equal(noRls, "0");
  const anon = psql(["-c", "select count(*) from information_schema.role_table_grants where table_schema = 'railyard' and grantee = 'anon'"]).trim();
  assert.equal(anon, "0");
  // Replay protection is a database guarantee, not only application code.
  psql(["-c", "insert into railyard.billing_events (webhook_id, event_type, outcome) values ('dodo:m1', 'payment.succeeded', 'applied')"]);
  assert.throws(() => psql(["-c", "insert into railyard.billing_events (webhook_id, event_type, outcome) values ('dodo:m1', 'payment.succeeded', 'applied')"]), /duplicate key/);
});
