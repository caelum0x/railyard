// Capacity (A3 acceptance 6): the service's resident memory, measured from
// outside with ps, while it takes 50 events/s (A1's traffic), recording
// ingest from 20 browsing sessions that flush every 2 s (the recorder's
// rhythm) and delivers one webhook export. Also: stored recording bytes per
// recorded minute on the fixture (fixtures/recording.ts).
//   node test/a3/capacity.ts            (CAPACITY_SECONDS=600 by default)
import { fork, execFileSync } from "node:child_process";
import { randomBytes, randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import type { AddressInfo } from "node:net";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { gzipSync, strToU8 } from "fflate";
import { migrate } from "../../scripts/migrate.ts";
import { signConsoleToken } from "../../src/tenancy/console-token.ts";
import { makeSite, scratchPg, sleep } from "../a1/helpers.ts";
import { recordingSlice } from "./fixtures/recording.ts";

const SECONDS = Number(process.env.CAPACITY_SECONDS ?? "600");
const RATE = 50;
const SESSIONS = 20;
const SESSION_SECONDS = 300;
const CAP_MIB = 192;
const HERE = dirname(fileURLToPath(import.meta.url));
const SECRET = randomBytes(24).toString("hex");

const pg = scratchPg();
await migrate(pg.url);
const store = mkdtempSync(join(tmpdir(), "pa-a3-store-"));
const site = makeSite({ events_limit: 10_000_000 });
let exported = 0;
const hook = createServer((req, res) => {
  const parts: Buffer[] = [];
  req.on("data", (d) => parts.push(d));
  req.on("end", () => {
    try {
      exported += (JSON.parse(Buffer.concat(parts).toString("utf8")).events ?? []).length;
    } catch {
      /* ignore */
    }
    res.writeHead(204).end();
  });
});
await new Promise<void>((r) => hook.listen(0, "127.0.0.1", () => r()));
const port = 20000 + Math.floor(Math.random() * 10000);
const child = fork(join(HERE, "capacity-server.ts"), [], {
  env: {
    ...process.env,
    DATABASE_URL: pg.url,
    PORT: String(port),
    CAPACITY_SITE: JSON.stringify(site),
    CAPACITY_CONSOLE_SECRET: SECRET,
    PA_OBJECT_STORE: "fs",
    PA_FS_STORE_DIR: store,
    PA_EXPORTS_KEY: randomBytes(32).toString("base64"),
    PA_EXPORTS_ALLOW_PRIVATE: "1",
    PA_INSIGHTS_JOBS: "off",
    NODE_OPTIONS: "--max-old-space-size=128",
  },
  stdio: ["ignore", "inherit", "inherit", "ipc"],
});
await new Promise((r) => child.once("message", r));
const base = `http://127.0.0.1:${port}`;
const rssMiB = () => Number(execFileSync("ps", ["-o", "rss=", "-p", String(child.pid)], { encoding: "utf8" }).trim()) / 1024;
const consoleCall = async (path: string, body: unknown, method = "POST") => {
  const tok = await signConsoleToken(SECRET, { org_id: site.org_id, app_ids: [site.project_id], role: "owner" }, new Date());
  const r = await fetch(`${base}/v1/projects/${site.project_id}${path}`, { method, headers: { authorization: `Bearer ${tok}` }, body: JSON.stringify(body) });
  if (!r.ok) throw new Error(`${path}: ${r.status} ${await r.text()}`);
};
await consoleCall("/recording-settings", { replay_enabled: true, replay_sample_rate: 1 }, "PUT");
await consoleCall("/exports", { name: "capacity", kind: "webhook", config: { url: `http://127.0.0.1:${(hook.address() as AddressInfo).port}/hook` } });

const seen = new Set<number>();
let sent = 0;
let failed = 0;
let snapshotRequests = 0;
let snapshotFailed = 0;
const samples: number[] = [];
const started = Date.now();
const eventBatch = () => {
  const events = Array.from({ length: 10 }, () => {
    const n = Math.floor(Math.random() * 200);
    const first = !seen.has(n);
    seen.add(n);
    return first
      ? { event: "$identify", distinct_id: `user-${n}`, uuid: randomUUID(), properties: { $anon_distinct_id: `anon-${n}`, $set: { plan: n % 3 ? "free" : "pro" } } }
      : { event: n % 5 ? "$pageview" : "clicked", distinct_id: `user-${n}`, uuid: randomUUID(), properties: { $current_url: `https://shop.example/p/${n % 50}`, $browser: "Chrome", amount: n } };
  });
  return gzipSync(strToU8(JSON.stringify({ api_key: site.token, batch: events, sent_at: new Date().toISOString() })));
};
const sessionIds = Array.from({ length: SESSIONS }, (_, i) => ({ id: `cap-${i}-${randomUUID()}`, startedAt: 0, seed: i }));
const snapshotBatch = (i: number, now: number) => {
  const s = sessionIds[i];
  const fresh = s.startedAt === 0 || now - s.startedAt > SESSION_SECONDS * 1000;
  if (fresh) Object.assign(s, { id: `cap-${i}-${randomUUID()}`, startedAt: now });
  const events = recordingSlice(s.seed, now - 2000, 2, fresh);
  return gzipSync(strToU8(JSON.stringify([{ event: "$snapshot", properties: { $snapshot_data: events, $session_id: s.id, $window_id: `w-${i}`, distinct_id: `user-${i}`, token: site.token } }])));
};

for (let s = 0; s < SECONDS; s += 1) {
  const tick = Date.now();
  const calls: Promise<void>[] = Array.from({ length: RATE / 10 }, () =>
    fetch(`${base}/batch/`, { method: "POST", headers: { "content-encoding": "gzip", "content-type": "application/json" }, body: eventBatch() }).then(
      (r) => void (r.ok || (failed += 10)),
      () => void (failed += 10),
    ),
  );
  sent += RATE;
  // Each session flushes every 2 s: half of them this second, half the next.
  for (let i = s % 2; i < SESSIONS; i += 2) {
    snapshotRequests += 1;
    calls.push(fetch(`${base}/s/?compression=gzip-js`, { method: "POST", body: snapshotBatch(i, Date.now()) }).then((r) => void (r.ok || (snapshotFailed += 1)), () => void (snapshotFailed += 1)));
  }
  await Promise.all(calls);
  if (s % 5 === 0) samples.push(rssMiB());
  const wait = 1000 - (Date.now() - tick);
  if (wait > 0) await sleep(wait);
}
await sleep(3000);
samples.push(rssMiB());
const stored = Number(pg.psql(`select count(*) from pa.events where project_id = '${site.project_id}'`));
const rec = pg.psql(`select coalesce(sum(size_bytes),0) || '|' || coalesce(sum(raw_bytes),0) || '|' || coalesce(sum(extract(epoch from ended_at - started_at)),0) || '|' || count(*) || '|' || coalesce(sum(chunk_count),0) from pa.replays where project_id = '${site.project_id}'`).split("|").map(Number);
const minutes = rec[2] / 60;
const result = {
  date: new Date().toISOString().slice(0, 10),
  seconds: Math.round((Date.now() - started) / 1000),
  events_sent: sent,
  events_failed: failed,
  events_stored: stored,
  snapshot_requests: snapshotRequests,
  snapshot_failed: snapshotFailed,
  recordings: rec[3],
  chunks: rec[4],
  recorded_minutes: Math.round(minutes * 10) / 10,
  stored_bytes_per_recorded_minute: minutes ? Math.round(rec[0] / minutes) : null,
  raw_bytes_per_recorded_minute: minutes ? Math.round(rec[1] / minutes) : null,
  exported_events: exported,
  rss_mib_max: Math.round(Math.max(...samples) * 10) / 10,
  rss_mib_last: Math.round(samples.at(-1)! * 10) / 10,
  rss_mib_first: Math.round(samples[0] * 10) / 10,
  cap_mib: CAP_MIB,
  within_cap: Math.max(...samples) <= CAP_MIB,
  node: process.version,
};
console.log(JSON.stringify(result));
child.kill("SIGTERM");
hook.close();
await sleep(1000);
pg.stop();
rmSync(store, { recursive: true, force: true });
process.exit(result.within_cap && stored === sent - failed && snapshotFailed === 0 && exported > 0 ? 0 : 1);
