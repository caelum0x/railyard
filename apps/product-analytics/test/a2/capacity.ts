// Capacity run for A2 (docs/analytics/CAPACITY.md): 1 000 000 generated
// events in one project on a scratch cluster, then each budgeted query 20
// times; prints p50/p95 and bytes per event as JSON.
//   node test/a2/capacity.ts [events=1000000] [runs=20]
// Uses the same generator as the tests. No hosted database is touched.

import { randomUUID } from "node:crypto";
import { cpus, loadavg } from "node:os";
import pg from "pg";
import { migrate } from "../../scripts/migrate.ts";
import { Db } from "../../src/db.ts";
import { runInsight } from "../../src/insights/runner.ts";
import { runRollups } from "../../src/insights/rollups.ts";
import { parseQuery } from "../../src/insights/validate.ts";
import { scratchPg } from "../a1/helpers.ts";
import { loadFixture } from "./fixtures.ts";

const target = Number(process.argv[2] ?? 1_000_000);
const runs = Number(process.argv[3] ?? 20);
const NOW = new Date("2026-09-30T00:00:00Z");
const DAYS = 90;

const cluster = scratchPg();
const out: Record<string, unknown> = { target_events: target, days: DAYS, runs, postgres: "", machine: `${process.platform} ${process.arch}, ${cpus().length} cores`, load_average_start: loadavg().map((x) => Math.round(x)) };
try {
  await migrate(cluster.url);
  const owner = new pg.Client({ connectionString: cluster.url });
  await owner.connect();
  out.postgres = (await owner.query("show server_version")).rows[0].server_version;
  const projectId = randomUUID();
  const t0 = Date.now();
  const stats = await loadFixture(owner, { projectId, persons: 10_000_000, maxEvents: target, days: DAYS, end: NOW, seed: 2026 });
  out.load_seconds = (Date.now() - t0) / 1000;
  out.events = stats.events;
  out.persons = stats.persons;
  await owner.query("vacuum analyze pa.events");
  const size = await owner.query<{ events_bytes: string; index_bytes: string; heap_bytes: string }>(
    `select sum(pg_total_relation_size(c.oid))::bigint as events_bytes,
            sum(pg_indexes_size(c.oid))::bigint as index_bytes,
            sum(pg_relation_size(c.oid))::bigint as heap_bytes
       from pg_inherits i join pg_class c on c.oid = i.inhrelid join pg_class p on p.oid = i.inhparent
      where p.relname = 'events'`,
  );
  const bytes = Number(size.rows[0].events_bytes);
  out.bytes_per_event = Math.round(bytes / stats.events);
  out.heap_bytes_per_event = Math.round(Number(size.rows[0].heap_bytes) / stats.events);
  out.index_bytes_per_event = Math.round(Number(size.rows[0].index_bytes) / stats.events);
  const db = new Db(cluster.url, 3);
  const tR = Date.now();
  await runRollups(db, new Date(), { settleMs: 0 });
  out.rollup_first_run_seconds = (Date.now() - tR) / 1000;
  const budgets: Record<string, { query: Record<string, unknown>; budget_ms: number }> = {
    trends_30d_total: { query: { kind: "trends", date_range: { date_from: "-30d" }, series: [{ event: "$pageview" }] }, budget_ms: 1500 },
    trends_30d_unique: { query: { kind: "trends", date_range: { date_from: "-30d" }, series: [{ event: "$pageview", math: "dau" }] }, budget_ms: 1500 },
    trends_30d_breakdown: { query: { kind: "trends", date_range: { date_from: "-30d" }, series: [{ event: "$pageview", math: "dau" }], breakdown: { type: "person", key: "country" } }, budget_ms: 1500 },
    funnel_3_steps_14d: {
      query: { kind: "funnels", date_range: { date_from: "-30d" }, steps: [{ event: "$pageview" }, { event: "signup" }, { event: "purchase" }], window: { value: 14, unit: "day" } },
      budget_ms: 3000,
    },
    retention_8_weeks: { query: { kind: "retention", date_range: { date_from: "-8w" }, period: "week", intervals: 8, start: { event: "signup" }, return: { event: "$pageview" } }, budget_ms: 3000 },
    lifecycle_30d: { query: { kind: "lifecycle", date_range: { date_from: "-30d" }, series: [{ event: "$pageview" }] }, budget_ms: 3000 },
    stickiness_30d: { query: { kind: "stickiness", date_range: { date_from: "-30d" }, series: [{ event: "$pageview" }] }, budget_ms: 3000 },
    paths_30d: { query: { kind: "paths", date_range: { date_from: "-30d" }, path_type: "pageview", depth: 5 }, budget_ms: 3000 },
  };
  const results: Record<string, unknown> = {};
  for (const [name, b] of Object.entries(budgets)) {
    const q = parseQuery(b.query);
    const times: number[] = [];
    for (let i = 0; i < runs; i += 1) {
      const s = performance.now();
      await runInsight(db, projectId, q, NOW);
      times.push(performance.now() - s);
    }
    times.sort((a, z) => a - z);
    const p = (x: number) => Math.round(times[Math.min(times.length - 1, Math.ceil(x * times.length) - 1)]);
    results[name] = { p50_ms: p(0.5), p95_ms: p(0.95), max_ms: Math.round(times[times.length - 1]), budget_ms: b.budget_ms, within: p(0.95) < b.budget_ms };
    process.stderr.write(`${name}: ${JSON.stringify(results[name])}\n`);
  }
  out.queries = results;
  out.load_average_end = loadavg().map((x) => Math.round(x));
  out.rss_mib = Math.round(process.memoryUsage().rss / 1048576);
  await db.close();
  await owner.end();
} finally {
  cluster.stop();
}
process.stdout.write(`${JSON.stringify(out, null, 2)}\n`);
