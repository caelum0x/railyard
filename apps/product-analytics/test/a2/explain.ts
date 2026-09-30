// Prints the plan of one insight on the kept perf cluster (test/a2/perf-cluster.ts):
//   node test/a2/explain.ts '{"kind":"trends",...}' [work_mem]   (FULL=1 for the whole plan)
import pg from "pg";
import { buildTrendSeries } from "../../src/insights/trends.ts";
import { buildFunnel } from "../../src/insights/funnels.ts";
import { buildRetention } from "../../src/insights/retention.ts";
import { buildLifecycle, buildStickiness } from "../../src/insights/lifecycle.ts";
import { buildPaths } from "../../src/insights/paths.ts";
import { parseQuery } from "../../src/insights/validate.ts";
import { resolveRange } from "../../src/insights/sql.ts";
const url = `postgresql://postgres@localhost/postgres?host=${encodeURIComponent(process.env.TMPDIR + "pa-perf")}&port=45432`;
const projectId = "0e0e0e0e-0000-4000-8000-000000000001";
const now = new Date("2026-09-30T00:00:00Z");
const q: any = parseQuery(JSON.parse(process.argv[2]));
const ctx = { projectId, now, actions: new Map() };
const r = resolveRange(q.date_range, now);
const b = q.kind === "trends" ? buildTrendSeries(q, q.series[0], r, ctx) : q.kind === "funnels" ? buildFunnel(q, r, ctx) : q.kind === "retention" ? buildRetention(q, r, ctx) : q.kind === "lifecycle" ? buildLifecycle(q, r, ctx) : q.kind === "stickiness" ? buildStickiness(q, r, ctx, 0) : buildPaths(q, r, ctx);
const c = new pg.Client({ connectionString: url });
await c.connect();
await c.query("begin");
await c.query("set local role pa_app");
await c.query("select set_config('pa.project_id', $1, true)", [projectId]);
if (process.argv[3]) await c.query(`set local work_mem = '${process.argv[3]}'`);
const res = await c.query(`explain (analyze, buffers, format text) ${b.sql}`, b.params);
const lines = res.rows.map((x: any) => x["QUERY PLAN"]);
console.log(process.env.FULL ? lines.join("\n") : lines.filter((l: string) => /actual time|Execution|Sort Method|Disk|Planning/.test(l)).slice(0, 60).join("\n"));
await c.query("rollback");
await c.end();
