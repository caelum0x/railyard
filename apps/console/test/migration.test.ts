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
const BRIDGE = join(ROOT, "db/migrations/0002_scribase_bridge.sql");
const AI = join(ROOT, "db/migrations/0003_ai_gateway.sql");
const ORDER = join(ROOT, "db/migrations/0004_bridge_order.sql");
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

test("0002 (Scribase bridge) applies twice after 0001; add-on plan allowed; bindings unique; anon has no access", { skip: have ? false : "pg_ctl/initdb not installed" }, () => {
  const dir = mkdtempSync(join(tmpdir(), "ry-pg-"));
  const port = String(57000 + Math.floor(Math.random() * 2000));
  after(() => {
    spawnSync("pg_ctl", ["-D", join(dir, "data"), "-m", "immediate", "stop"], { stdio: "ignore" });
    rmSync(dir, { recursive: true, force: true });
  });
  execFileSync("initdb", ["-D", join(dir, "data"), "-U", "postgres", "-A", "trust", "--no-sync"], { stdio: "ignore" });
  execFileSync("pg_ctl", ["-D", join(dir, "data"), "-o", `-p ${port} -k ${dir} -c listen_addresses=''`, "-w", "start"], { stdio: "ignore" });
  const psql = (args: string[]) => execFileSync("psql", ["-h", dir, "-p", port, "-U", "postgres", "-v", "ON_ERROR_STOP=1", "-qAt", ...args], { encoding: "utf8" });
  psql(["-c", "create role anon nologin; create role authenticated nologin; create role service_role nologin bypassrls;"]);
  psql(["-f", MIGRATION]);
  psql(["-f", BRIDGE]);
  psql(["-f", MIGRATION]);
  psql(["-f", BRIDGE]);
  const org = psql(["-c", "insert into railyard.orgs (name, owner_user_id) values ('Acme', gen_random_uuid()) returning id"]).trim();
  psql(["-c", `insert into railyard.entitlements (org_id, plan, status, addon_slots) values ('${org}', 'addon', 'active', '{deploy,analytics}')`]);
  assert.throws(() => psql(["-c", `update railyard.entitlements set plan = 'free' where org_id = '${org}'`]), /entitlements_plan_check/);
  psql(["-c", `insert into railyard.scribase_bindings (scribase_org_id, org_id) values ('org_abc', '${org}')`]);
  assert.throws(() => psql(["-c", `insert into railyard.scribase_bindings (scribase_org_id, org_id) values ('org_other', '${org}')`]), /duplicate key/);
  assert.throws(() => psql(["-c", `insert into railyard.scribase_bindings (scribase_org_id, org_id) values ('bad id;', '${org}')`]), /check constraint/);
  psql(["-c", "insert into railyard.bridge_messages (message_id) values ('scribase:m1')"]);
  assert.throws(() => psql(["-c", "insert into railyard.bridge_messages (message_id) values ('scribase:m1')"]), /duplicate key/);
  const noRls = psql(["-c", "select count(*) from pg_tables where schemaname = 'railyard' and not rowsecurity"]).trim();
  assert.equal(noRls, "0");
  const anon = psql(["-c", "select count(*) from information_schema.role_table_grants where table_schema = 'railyard' and grantee in ('anon', 'authenticated')"]).trim();
  assert.equal(anon, "0");
});

test("0003 (AI gateway) applies twice; usage adds atomically; secrets are not readable by anon", { skip: have ? false : "pg_ctl/initdb not installed" }, () => {
  const dir = mkdtempSync(join(tmpdir(), "ry-pg-"));
  const port = String(59000 + Math.floor(Math.random() * 900));
  after(() => {
    spawnSync("pg_ctl", ["-D", join(dir, "data"), "-m", "immediate", "stop"], { stdio: "ignore" });
    rmSync(dir, { recursive: true, force: true });
  });
  execFileSync("initdb", ["-D", join(dir, "data"), "-U", "postgres", "-A", "trust", "--no-sync"], { stdio: "ignore" });
  execFileSync("pg_ctl", ["-D", join(dir, "data"), "-o", `-p ${port} -k ${dir} -c listen_addresses=''`, "-w", "start"], { stdio: "ignore" });
  const psql = (args: string[]) => execFileSync("psql", ["-h", dir, "-p", port, "-U", "postgres", "-v", "ON_ERROR_STOP=1", "-qAt", ...args], { encoding: "utf8" });
  psql(["-c", "create role anon nologin; create role authenticated nologin; create role service_role nologin bypassrls;"]);
  for (const f of [MIGRATION, BRIDGE, AI, MIGRATION, BRIDGE, AI]) psql(["-f", f]);
  const org = psql(["-c", "insert into railyard.orgs (name, owner_user_id) values ('Acme', gen_random_uuid()) returning id"]).trim();
  const key = psql(["-c", `insert into railyard.ai_keys (org_id, label, key_hash, daily_token_cap, monthly_token_cap) values ('${org}', 'shop', 'h1', 10, 100) returning id`]).trim();
  psql(["-c", `select railyard.ai_add_usage('${key}', '2026-09-29', 7); select railyard.ai_add_usage('${key}', '2026-09-29', 5);`]);
  assert.equal(psql(["-c", `select tokens || ',' || requests from railyard.ai_usage where key_id = '${key}'`]).trim(), "12,2");
  assert.throws(() => psql(["-c", `insert into railyard.ai_credentials (org_id, provider, nonce, ciphertext, key_hint) values ('${org}', 'ours', 'n', 'c', 'h')`]), /check constraint/);
  const anon = psql(["-c", "select count(*) from information_schema.role_table_grants where table_schema = 'railyard' and grantee in ('anon', 'authenticated')"]).trim();
  assert.equal(anon, "0");
  const exec = psql(["-c", "select has_function_privilege('anon', 'railyard.ai_add_usage(uuid, date, bigint)', 'execute')"]).trim();
  assert.equal(exec, "f");
});

test("0004 (bridge order, P-Y1) applies twice; the guarded update never moves an entitlement back", { skip: have ? false : "pg_ctl/initdb not installed" }, () => {
  const dir = mkdtempSync(join(tmpdir(), "ry-pg-"));
  const port = String(53000 + Math.floor(Math.random() * 1900));
  after(() => {
    spawnSync("pg_ctl", ["-D", join(dir, "data"), "-m", "immediate", "stop"], { stdio: "ignore" });
    rmSync(dir, { recursive: true, force: true });
  });
  execFileSync("initdb", ["-D", join(dir, "data"), "-U", "postgres", "-A", "trust", "--no-sync"], { stdio: "ignore" });
  execFileSync("pg_ctl", ["-D", join(dir, "data"), "-o", `-p ${port} -k ${dir} -c listen_addresses=''`, "-w", "start"], { stdio: "ignore" });
  const psql = (args: string[]) => execFileSync("psql", ["-h", dir, "-p", port, "-U", "postgres", "-v", "ON_ERROR_STOP=1", "-qAt", ...args], { encoding: "utf8" });
  psql(["-c", "create role anon nologin; create role authenticated nologin; create role service_role nologin bypassrls;"]);
  for (const f of [MIGRATION, BRIDGE, AI, ORDER, MIGRATION, BRIDGE, AI, ORDER]) psql(["-f", f]);
  const org = psql(["-c", "insert into railyard.orgs (name, owner_user_id) values ('Acme', gen_random_uuid()) returning id"]).trim();
  psql(["-c", `insert into railyard.entitlements (org_id, plan, status, addon_as_of) values ('${org}', 'addon', 'inactive', 2000)`]);
  // The statement PostgrestRepo.saveAddonEntitlement sends (PostgREST or=(addon_as_of.is.null,addon_as_of.lte.N)).
  const guarded = (status: string, asOf: number) =>
    psql(["-c", `update railyard.entitlements set status = '${status}', addon_as_of = ${asOf} where org_id = '${org}' and (addon_as_of is null or addon_as_of <= ${asOf}) returning status`]).trim();
  assert.equal(guarded("active", 1500), "", "an older state changes nothing");
  assert.equal(psql(["-c", `select status from railyard.entitlements where org_id = '${org}'`]).trim(), "inactive");
  assert.equal(guarded("active", 2500), "active");
  assert.throws(() => psql(["-c", `update railyard.entitlements set addon_as_of = -1 where org_id = '${org}'`]), /entitlements_addon_as_of_check/);
});
