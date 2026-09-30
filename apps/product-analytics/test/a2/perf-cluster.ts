// A kept scratch cluster with the 1 000 000-event fixture, for looking at
// query plans while tuning (node test/a2/perf-cluster.ts start|stop).
// Lives in $TMPDIR/pa-perf; never a hosted database.
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import pg from "pg";
import { migrate } from "../../scripts/migrate.ts";
import { loadFixture } from "./fixtures.ts";

const dir = join(tmpdir(), "pa-perf");
const port = "45432";
const url = `postgresql://postgres@localhost/postgres?host=${encodeURIComponent(dir)}&port=${port}`;
const cmd = process.argv[2] ?? "start";
if (cmd === "stop") {
  spawnSync("pg_ctl", ["-D", join(dir, "data"), "-m", "fast", "stop"], { stdio: "ignore" });
  rmSync(dir, { recursive: true, force: true });
  process.exit(0);
}
if (!existsSync(join(dir, "data"))) {
  mkdirSync(dir, { recursive: true });
  execFileSync("initdb", ["-D", join(dir, "data"), "-U", "postgres", "-A", "trust", "--no-sync"], { stdio: "ignore" });
}
execFileSync("pg_ctl", ["-D", join(dir, "data"), "-o", `-p ${port} -k ${dir} -c listen_addresses='' -c timezone=UTC`, "-w", "start"], { stdio: "ignore" });
await migrate(url);
const c = new pg.Client({ connectionString: url });
await c.connect();
const n = Number((await c.query("select count(*) as n from pa.events")).rows[0].n);
if (n === 0) {
  const projectId = "0e0e0e0e-0000-4000-8000-000000000001";
  const s = await loadFixture(c, { projectId, persons: 10_000_000, maxEvents: Number(process.argv[3] ?? 1_000_000), days: 90, end: new Date("2026-09-30T00:00:00Z"), seed: 2026 });
  await c.query("vacuum analyze");
  writeFileSync(join(dir, "stats.json"), JSON.stringify(s));
}
await c.end();
process.stdout.write(`${url}\n`);
