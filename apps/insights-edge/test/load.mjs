// Load test (RYP-D3 acceptance 4): 1,000 events a second for 60 s against the
// collector in Miniflare (workerd). Every call must return 202, and Insights
// ingest must see at most one request a second for any site.
//   node test/load.mjs [seconds=60] [rate=1000]
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { ev, sleep, startCollector } from "./harness.mjs";

const SECONDS = Number(process.argv[2] ?? 60);
const RATE = Number(process.argv[3] ?? 1000);
const SITES = Array.from({ length: 5 }, (_, i) => `ry_site_load${String(i).padStart(8, "0")}`);
const sites = Object.fromEntries(SITES.map((k, i) => [k, { org_id: `org-${i % 3}`, app_id: `app-${i}`, events_limit: 10_000_000, paused: false }]));
const c = await startCollector({ sites });

const statuses = new Map();
let sent = 0;
const t0 = Date.now();
let late = 0;
for (let s = 0; s < SECONDS; s += 1) {
  const tick = t0 + s * 1000;
  const wait = tick - Date.now();
  if (wait > 0) await sleep(wait);
  else late += 1;
  // 64 connections in flight, like many browsers behind one edge.
  const jobs = Array.from({ length: RATE }, (_, i) => i);
  const worker = async () => {
    for (let i = jobs.shift(); i !== undefined; i = jobs.shift()) {
      const site = SITES[i % SITES.length];
      const r = await c.send(ev(site, `/p/${s}/${i}`), { "cf-connecting-ip": `198.51.100.${i % 250}` });
      statuses.set(r.status, (statuses.get(r.status) ?? 0) + 1);
      await r.arrayBuffer();
      sent += 1;
    }
  };
  await Promise.all(Array.from({ length: 64 }, worker));
}
const elapsed = (Date.now() - t0) / 1000;
await sleep(3000);

// Downstream: requests per site per 1-second window.
const ingest = c.outbound.filter((o) => o.url.includes("/v1/ingest/batch"));
const perSite = new Map();
let forwarded = 0;
for (const o of ingest) {
  const body = JSON.parse(o.body);
  for (const b of body.batches) {
    forwarded += b.events.length;
    if (!perSite.has(b.site_key)) perSite.set(b.site_key, []);
    perSite.get(b.site_key).push(o.at);
  }
}
let maxPerSecond = 0;
for (const times of perSite.values()) {
  times.sort((a, b) => a - b);
  for (let i = 0; i < times.length; i += 1) {
    let j = i;
    while (j + 1 < times.length && times[j + 1] - times[i] < 1000) j += 1;
    maxPerSecond = Math.max(maxPerSecond, j - i + 1);
  }
}
const all202 = statuses.get(202) === sent && statuses.size === 1;
const leakedIp = c.outbound.some((o) => o.body.includes("198.51.100."));
const result = {
  date: new Date().toISOString(),
  runtime: "Miniflare 5 (workerd 1.20260923.1), local Mac, other lanes' builds running",
  target: `${RATE} events/s for ${SECONDS} s over ${SITES.length} sites in 3 orgs`,
  sent,
  elapsed_s: Number(elapsed.toFixed(1)),
  achieved_rate: Math.round(sent / elapsed),
  late_seconds: late,
  statuses: Object.fromEntries(statuses),
  all_202: all202,
  downstream_requests: ingest.length,
  events_forwarded: forwarded,
  max_downstream_requests_per_site_per_second: maxPerSecond,
  raw_ip_downstream: leakedIp,
};
await c.close();
console.log(JSON.stringify(result, null, 2));
if (process.env.LOAD_RESULT_FILE) {
  writeFileSync(join(dirname(fileURLToPath(import.meta.url)), process.env.LOAD_RESULT_FILE), `# Insights edge load test\n\nRun by \`node test/load.mjs ${SECONDS} ${RATE}\`.\n\n\`\`\`json\n${JSON.stringify(result, null, 2)}\n\`\`\`\n`);
}
const ok = all202 && maxPerSecond <= 1 && forwarded === sent && !leakedIp;
process.exit(ok ? 0 : 1);
