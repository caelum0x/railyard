// The console side of insights: the builder form maps to queries the
// service accepts; the client reads every endpoint through a real service;
// chart geometry is right; every chart and screen renders to HTML with no
// new dependency; the screens pass the copy denylist, name no upstream
// product, use no emoji and offer one primary action each.
import { before, after, test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import pg from "pg";
import ts from "typescript";
import { parseQuery } from "../../src/insights/validate.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running } from "../a1/helpers.ts";
import { loadFixture } from "./fixtures.ts";
import { NOW } from "./setup.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const RY = join(HERE, "../../../..");
const CONSOLE = join(RY, "apps/console");
const PA_UI = join(CONSOLE, "app/(app)/product-analytics");
const MINE = ["insights", "dashboards", "cohorts", "sql", "actions", "_components/charts", "_lib/insights-client.ts"].map((p) => join(PA_UI, p));
const SECRET = "c".repeat(48);

const client = await import(join(PA_UI, "_lib/insights-client.ts"));
const geo = await import(join(PA_UI, "_components/charts/geometry.ts"));
const palette = await import(join(PA_UI, "_components/charts/palette.ts"));
const lint = await import(join(RY, "policy/copy-lint.mjs"));

const walk = (p: string): string[] => (statSync(p).isDirectory() ? readdirSync(p).flatMap((f) => walk(join(p, f))) : [p]);
const myFiles = () => MINE.flatMap(walk).filter((f) => /\.(tsx?|mjs)$/.test(f));

test("builder forms map to queries the service accepts, and back", () => {
  const forms = [
    { kind: "trends", e1: "$pageview", m1: "dau", e2: "signup", breakdown: "person:plan", formula: "B / A", compare: "on", interval: "week", range: "-90d" },
    { kind: "trends", e1: "purchase", m1: "sum", p1: "amount", segment: "11111111-1111-4111-8111-111111111111", pkey: "plan", pval: "pro" },
    { kind: "funnels", e1: "$pageview", e2: "signup", e3: "purchase", order: "strict", window: "3", wunit: "hour", breakdown: "event:source" },
    { kind: "retention", e1: "signup", e2: "$pageview", period: "month", intervals: "6", rtype: "recurring" },
    { kind: "paths", ptype: "screen", start: "Home", end: "Checkout", depth: "4" },
    { kind: "stickiness", e1: "$pageview", interval: "day" },
    { kind: "lifecycle", e1: "action:22222222-2222-4222-8222-222222222222", interval: "week" },
    { kind: "nonsense" },
  ];
  for (const f of forms) {
    const q = client.formToQuery(f);
    const parsed = parseQuery(q);
    assert.equal(parsed.kind, f.kind === "nonsense" ? "trends" : f.kind);
    if (f.kind !== "nonsense") {
      const again = client.formToQuery(client.queryToForm(q));
      assert.deepEqual(again, q, f.kind);
    }
  }
  const href = client.builderHref(client.formToQuery(forms[0]), "shop");
  assert.match(href, /^\/product-analytics\/insights\/new\?app=shop&kind=trends/);
});

test("chart geometry: clean ticks, rounded columns, compact numbers, readable ink", () => {
  assert.deepEqual(geo.niceTicks(0, 87), [0, 50, 100]);
  assert.deepEqual(geo.niceTicks(-30, 12, 4), [-40, -20, 0, 20]);
  assert.deepEqual(geo.niceTicks(0, 0), [0, 0.5, 1]);
  const y = geo.linearScale([0, 50, 100], 200, 0);
  assert.equal(y(0), 200);
  assert.equal(y(100), 0);
  assert.equal(geo.columnPath(10, 20, 100, 100), "");
  assert.match(geo.columnPath(10, 20, 100, 40), /^M10,100V44Q10,40 14,40H26Q30,40 30,44V100Z$/);
  assert.match(geo.columnPath(10, 20, 100, 160), /V156/);
  assert.equal(geo.compact(1284), "1,284");
  assert.equal(geo.compact(12_900), "12.9K");
  assert.equal(geo.compact(4_200_000), "4.2M");
  assert.equal(geo.compact(0.456), "0.46");
  assert.equal(geo.percent(0.5), "50%");
  assert.equal(geo.percent(0.034), "3.4%");
  assert.equal(geo.duration(45), "45 s");
  assert.equal(geo.duration(600), "10 min");
  assert.equal(geo.duration(7200), "2.0 h");
  assert.equal(geo.duration(3 * 86400), "3.0 d");
  assert.equal(geo.bucketText("2026-09-03", "day"), "Sep 3");
  assert.equal(geo.bucketText("2026-09-01", "month"), "Sep 2026");
  assert.equal(geo.bucketText("2026-09-03 14:00", "hour"), "14:00");
  assert.deepEqual(geo.labelIndexes(31, 6), [0, 6, 12, 18, 24, 30]);
  assert.equal(geo.nearestIndex([0, 10, 20], 14), 1);
  assert.equal(geo.inkOn("#0d366b"), "#ffffff");
  assert.equal(geo.inkOn("#cde2fb"), "#0b0b0b");
  assert.equal(palette.SERIES.length, 8);
  assert.equal(palette.seriesColor(20), palette.SERIES[7], "a ninth series never gets a generated hue");
  assert.equal(palette.sequentialFor(0), palette.INK.surface);
  assert.equal(palette.sequentialFor(1), palette.SEQUENTIAL[palette.SEQUENTIAL.length - 1]);
});

let run: Running;
const site = makeSite({ name: "shop" });
before(async () => {
  if (skipNoPg) return;
  const scratch = await migratedPg();
  const owner = new pg.Client({ connectionString: scratch.url });
  await owner.connect();
  await loadFixture(owner, { projectId: site.project_id, persons: 60, days: 30, end: NOW, seed: 3 });
  await owner.end();
  run = await startService(scratch, [site], { consoleSecret: SECRET, now: () => NOW });
});

test("the console client reads every insights endpoint through the service", { skip: skipNoPg }, async () => {
  const cfg = { url: run.base, secret: SECRET };
  const api = client.insightsApi(cfg, { org_id: site.org_id, app_ids: [site.project_id] }, site.project_id);
  const meta = await api.meta();
  assert.ok(meta.events.length > 0);
  for (const kind of ["trends", "funnels", "retention", "paths", "stickiness", "lifecycle"]) {
    const a = await api.query(client.formToQuery({ kind, range: "-30d" }));
    assert.equal(a.result.kind, kind);
  }
  const { insight } = await api.save("Views", client.formToQuery({ kind: "trends" }));
  assert.equal((await api.savedOne(insight.id)).insight.name, "Views");
  const { dashboard } = await api.createDashboard("Main");
  await api.addTile(dashboard.id, insight.id);
  assert.equal((await api.dashboard(dashboard.id)).dashboard.tiles.length, 1);
  const people = await api.funnelPeople(client.formToQuery({ kind: "funnels" }), 2, true);
  assert.ok(Array.isArray(people.people));
  const { segment } = await api.createCohort({ name: "File", kind: "static" });
  assert.equal((await api.uploadCohort(segment.id, "distinct_id\nuser_1\n")).segment.people_count, 1);
  assert.equal((await api.createAction("Buy", [{ event: "$autocapture", selector: "button" }])).action.name, "Buy");
  assert.equal((await api.sql("select count(*) as n from pa_view.events")).columns[0], "n");
  const csv = await api.sqlCsv("select 1 as one");
  assert.equal(await csv.text(), "one\r\n1\r\n");
  await assert.rejects(api.sql("delete from pa_view.events"), /Only SELECT/);
  await assert.rejects(client.insightsApi(cfg, { org_id: site.org_id, app_ids: [site.project_id] }, "00000000-0000-4000-8000-000000000000").meta(), /not in your organization/);
});

// Rendering: transpile the chart components (TSX) with the TypeScript
// compiler already in this package and render them with the console's own
// React, so no dependency is added anywhere.
const REACT_DOM = join(CONSOLE, "node_modules/react-dom/server.js");
const renderable = existsSync(REACT_DOM);
const OUT = join(CONSOLE, ".a2-render-test");
after(() => rmSync(OUT, { recursive: true, force: true }));

async function loadCharts() {
  rmSync(OUT, { recursive: true, force: true });
  mkdirSync(join(OUT, "charts"), { recursive: true });
  mkdirSync(join(OUT, "_lib"), { recursive: true });
  const chartDir = join(PA_UI, "_components/charts");
  for (const f of readdirSync(chartDir)) {
    const js = ts.transpileModule(readFileSync(join(chartDir, f), "utf8"), { compilerOptions: { jsx: ts.JsxEmit.ReactJSX, module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 } }).outputText;
    writeFileSync(join(OUT, "charts", f.replace(/\.tsx?$/, ".mjs")), js.replace(/from "(\.[^"]+)\.tsx?"/g, 'from "$1.mjs"').replace(/"\.\.\/\.\.\/_lib\/insights-client\.mjs"/g, '"../_lib/insights-client.mjs"'));
  }
  writeFileSync(join(OUT, "_lib", "insights-client.mjs"), "export {};\n");
  const React = await import(pathToFileURL(join(CONSOLE, "node_modules/react/index.js")).href);
  const server = await import(pathToFileURL(REACT_DOM).href);
  const view = await import(pathToFileURL(join(OUT, "charts", "ResultView.mjs")).href);
  return { React: React.default ?? React, render: (server.default ?? server).renderToStaticMarkup as (el: unknown) => string, ResultView: view.ResultView };
}

test("every insight result renders as an SVG or table chart with a table view", { skip: renderable ? false : "console dependencies not linked" }, async () => {
  const { React, render, ResultView } = await loadCharts();
  const labels = ["2026-09-01", "2026-09-02", "2026-09-03"];
  const results = [
    { kind: "trends", interval: "day", labels, series: [{ label: "Views", series_index: 0, breakdown: null, data: [3, 5, 4], total: 12 }], previous: { labels, series: [{ label: "Views", series_index: 0, breakdown: null, data: [1, 2, 3], total: 6 }] } },
    { kind: "trends", interval: "day", labels, series: Array.from({ length: 10 }, (_, i) => ({ label: "Views", series_index: 0, breakdown: `v${i}`, data: [i, i, i], total: 3 * i })) },
    { kind: "funnels", groups: [{ breakdown: null, breakdown_value: null, steps: [{ step: 1, name: "view", people: 10, from_first: 1, from_previous: 1, dropped: 0, avg_seconds: null, median_seconds: null }, { step: 2, name: "buy", people: 4, from_first: 0.4, from_previous: 0.4, dropped: 6, avg_seconds: 60, median_seconds: 50 }] }] },
    { kind: "retention", period: "week", cohorts: [{ label: "2026-09-01", size: 10, values: [10, 4, 2] }, { label: "2026-09-08", size: 0, values: [0, 0] }] },
    { kind: "paths", edges: [{ source_step: 1, source: "/a", target: "/b", people: 5, avg_seconds: 30, ends: 1 }] },
    { kind: "stickiness", interval: "day", series: [{ label: "Views", data: [5, 2, 1] }] },
    { kind: "lifecycle", interval: "day", labels, statuses: { new: [1, 2, 0], returning: [0, 1, 1], resurrecting: [0, 0, 1], dormant: [0, -1, -2] } },
  ];
  for (const r of results) {
    const html = render(React.createElement(ResultView, { result: r, dropOffBase: "/product-analytics/insights/people?app=shop" }));
    assert.ok(html.includes("<svg") || html.includes("<table"), r.kind);
    assert.ok(html.includes("Show as a table") || r.kind === "retention", `${r.kind} has a table view`);
    assert.doesNotMatch(html, /NaN|undefined|Infinity/, r.kind);
  }
  const trend = render(React.createElement(ResultView, { result: results[0] }));
  assert.match(trend, /Previous period/);
  assert.match(trend, /stroke-width="2"/);
  const many = render(React.createElement(ResultView, { result: results[1] }));
  assert.match(many, /first 8 lines/);
  const funnel = render(React.createElement(ResultView, { result: results[2], dropOffBase: "/x?app=shop" }));
  assert.match(funnel, /href="\/x\?app=shop&amp;step=2"/);
  assert.match(funnel, /6 dropped off before this step/);
  const life = render(React.createElement(ResultView, { result: results[6] }));
  for (const s of ["New", "Returning", "Resurrecting", "Dormant"]) assert.match(life, new RegExp(s));
});

test("the screens pass the copy denylist, name no upstream product, use no emoji and keep one primary action", () => {
  const files = myFiles();
  assert.ok(files.length >= 20, `found ${files.length} files`);
  const units = files.flatMap((f) => {
    const src = readFileSync(f, "utf8");
    const where = relative(RY, f);
    assert.doesNotMatch(src, /posthog|rrweb|umami|mixpanel|amplitude|hogql/i, where);
    assert.doesNotMatch(src, /\bindie\b/i, where);
    assert.doesNotMatch(src, /[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}]/u, where);
    if (/page\.tsx$/.test(f)) assert.ok((src.match(/className="primary"/g) ?? []).length <= 1, `${where}: one primary action`);
    const strings = [...src.matchAll(/>([^<>{}]+)</g), ...src.matchAll(/"([^"\n]{3,})"/g), ...src.matchAll(/`([^`\n]{3,})`/g)].map((m) => m[1]);
    return strings.map((t) => ({ text: t, surface: "app-ui", where, path: where }));
  });
  const found = lint.errors(lint.lintUnits(units));
  assert.deepEqual(found, [], lint.formatFindings(found));
});

test("no new console dependency and no edits outside the insights paths", () => {
  // Checked per product analytics commit, not on `git diff main`: once this
  // lane is merged, main moves and other lanes' edits would show up.
  const guarded = ["apps/console/package.json", "apps/console/lib", "apps/insights/src", "packages/insights-web", "packages/insights-react-native", "apps/console/app/(app)/insights"];
  const git = (...args: string[]) => execFileSync("git", ["-C", RY, ...args], { encoding: "utf8" }).trim();
  const commits = git("rev-list", "--no-merges", "HEAD", "--", "apps/product-analytics").split("\n").filter(Boolean);
  assert.ok(commits.length > 0);
  const touched = commits.flatMap((c) => git("show", "--name-only", "--format=", c, "--", ...guarded).split("\n").filter(Boolean).map((f) => `${c.slice(0, 7)} ${f}`));
  assert.deepEqual(touched, []);
});
