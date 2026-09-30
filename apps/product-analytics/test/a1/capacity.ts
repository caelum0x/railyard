// Capacity (A1 acceptance 10): the service's resident memory at 50 events
// per second for 10 minutes, measured from outside the process with ps.
//   node test/a1/capacity.ts            (CAPACITY_SECONDS=600 by default)
// Traffic: 5 gzip batches of 10 events per second over 200 people, with an
// identify for each new person and a group on every event.
import { fork, execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync, strToU8 } from "fflate";
import { migrate } from "../../scripts/migrate.ts";
import { makeSite, scratchPg, sleep } from "./helpers.ts";

const SECONDS = Number(process.env.CAPACITY_SECONDS ?? "600");
const RATE = 50;
const CAP_MIB = 192;
const HERE = dirname(fileURLToPath(import.meta.url));

const pg = scratchPg();
await migrate(pg.url);
const site = makeSite({ events_limit: 10_000_000 });
const port = 20000 + Math.floor(Math.random() * 10000);
const child = fork(join(HERE, "capacity-server.ts"), [], {
  env: { ...process.env, DATABASE_URL: pg.url, PORT: String(port), CAPACITY_SITE: JSON.stringify(site), NODE_OPTIONS: "--max-old-space-size=128" },
  stdio: ["ignore", "inherit", "inherit", "ipc"],
});
await new Promise((r) => child.once("message", r));
const base = `http://127.0.0.1:${port}`;
const rssMiB = () => Number(execFileSync("ps", ["-o", "rss=", "-p", String(child.pid)], { encoding: "utf8" }).trim()) / 1024;

const seen = new Set<number>();
let sent = 0;
let failed = 0;
const samples: number[] = [];
const started = Date.now();
const batch = () => {
  const events = Array.from({ length: 10 }, () => {
    const n = Math.floor(Math.random() * 200);
    const first = !seen.has(n);
    seen.add(n);
    return first
      ? { event: "$identify", distinct_id: `user-${n}`, uuid: randomUUID(), properties: { $anon_distinct_id: `anon-${n}`, $set: { plan: n % 3 ? "free" : "pro" }, $groups: { company: `c${n % 20}` } } }
      : { event: n % 5 ? "$pageview" : "clicked", distinct_id: `user-${n}`, uuid: randomUUID(), properties: { $current_url: `https://shop.example/p/${n % 50}`, $browser: "Chrome", $groups: { company: `c${n % 20}` }, amount: n } };
  });
  return gzipSync(strToU8(JSON.stringify({ api_key: site.token, batch: events, sent_at: new Date().toISOString() })));
};

for (let s = 0; s < SECONDS; s += 1) {
  const tick = Date.now();
  const res = await Promise.all(Array.from({ length: RATE / 10 }, () => fetch(`${base}/batch/`, { method: "POST", headers: { "content-encoding": "gzip", "content-type": "application/json" }, body: batch() }).then((r) => r.ok, () => false)));
  sent += RATE;
  failed += res.filter((ok) => !ok).length * 10;
  if (s % 5 === 0) samples.push(rssMiB());
  const wait = 1000 - (Date.now() - tick);
  if (wait > 0) await sleep(wait);
}
await sleep(3000);
samples.push(rssMiB());
const stored = Number(pg.psql(`select count(*) from pa.events where project_id = '${site.project_id}'`));
const result = {
  date: new Date().toISOString().slice(0, 10),
  seconds: Math.round((Date.now() - started) / 1000),
  events_sent: sent,
  events_failed: failed,
  events_stored: stored,
  rss_mib_max: Math.round(Math.max(...samples) * 10) / 10,
  rss_mib_last: Math.round(samples.at(-1)! * 10) / 10,
  rss_mib_first: Math.round(samples[0] * 10) / 10,
  cap_mib: CAP_MIB,
  within_cap: Math.max(...samples) <= CAP_MIB,
  node: process.version,
};
console.log(JSON.stringify(result));
child.kill("SIGTERM");
await sleep(1000);
pg.stop();
process.exit(result.within_cap && stored === sent - failed ? 0 : 1);
