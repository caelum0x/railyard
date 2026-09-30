// Click maps and scroll depth: fixture `$$heatmap` events (the browser SDK's
// shape) aggregate to exactly the hand count per bucket, type and viewport
// filter; scroll depth counts each page view once; another project's points
// never show; the overlay page opens from a signed link and, in Chromium,
// draws the points over the page frame.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { createServer } from "node:http";
import { existsSync } from "node:fs";
import type { AddressInfo } from "node:net";
import { chromium } from "playwright-core";
import { X_BUCKETS, Y_BUCKET_PX } from "../../src/heatmaps/query.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { api, insertEvents, SECRET, type Row } from "./helpers.ts";

let pg: ScratchPg;
let run: Running;
let pageUrl = "";
const site = makeSite({ name: "shop" });
const other = makeSite({ name: "other" });
const now = new Date();
const ago = (min: number) => new Date(now.getTime() - min * 60_000);

type Pt = { x: number; y: number; type: string; target_fixed?: boolean };
const heat = (distinct: string, vw: number, byUrl: Record<string, Pt[]>, at = ago(30)): Row => ({
  event: "$$heatmap",
  distinct_id: distinct,
  timestamp: at,
  properties: { $heatmap_data: Object.fromEntries(Object.entries(byUrl).map(([u, pts]) => [u, pts.map((p) => ({ target_fixed: false, ...p }))])), $viewport_width: vw, $viewport_height: 800 },
});

const hand = new Map<string, number>();
const key = (x: number, vw: number, y: number) => `${Math.min(X_BUCKETS, Math.round((x / vw) * X_BUCKETS)) / X_BUCKETS}|${Math.floor(y / Y_BUCKET_PX) * Y_BUCKET_PX}`;

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site, other], { consoleSecret: SECRET, now: () => now });
  const srv = createServer((_req, res) => res.writeHead(200, { "content-type": "text/html" }).end("<!doctype html><html><body style='margin:0'><h1>Pricing</h1><button style='margin:200px'>Buy</button></body></html>"));
  await new Promise<void>((r) => srv.listen(0, "127.0.0.1", () => r()));
  srv.unref();
  pageUrl = `http://127.0.0.1:${(srv.address() as AddressInfo).port}/pricing`;
  const rows: Row[] = [];
  for (let i = 0; i < 60; i += 1) {
    const vw = i % 3 === 0 ? 1280 : i % 3 === 1 ? 1440 : 390;
    const pts: Pt[] = [
      { x: 0.5 * vw, y: 216, type: "click" },
      { x: (0.1 + (i % 5) * 0.05) * vw, y: 40 + (i % 4) * 16, type: "click" },
      { x: 0.9 * vw, y: 1000, type: "mousemove" },
    ];
    if (i % 10 === 0) pts.push({ x: 0.5 * vw, y: 216, type: "rageclick" });
    rows.push(heat(`v${i}`, vw, { [pageUrl]: pts, "https://shop.example/other": [{ x: 1, y: 1, type: "click" }] }));
    for (const p of pts) if (p.type === "click") hand.set(key(p.x, vw, p.y), (hand.get(key(p.x, vw, p.y)) ?? 0) + 1);
  }
  rows.push(heat("old", 1280, { [pageUrl]: [{ x: 10, y: 10, type: "click" }] }, ago(60 * 24 * 10)));
  rows.push({ event: "$$heatmap", distinct_id: "bad", timestamp: ago(5), properties: { $heatmap_data: { [pageUrl]: [{ x: "x", y: 3, type: "click" }] }, $viewport_width: "wide" } });
  // Scroll depth: 4 page views of /pricing; one of them reported twice (page leave, then the next page view).
  const u = new URL(pageUrl);
  const scroll = (distinct: string, id: string, pct: number, event = "$pageleave"): Row => ({
    event,
    distinct_id: distinct,
    timestamp: ago(20),
    properties: { $host: u.host, $prev_pageview_pathname: u.pathname, $prev_pageview_id: id, $prev_pageview_max_scroll_percentage: pct, $prev_pageview_max_content: 1200 * Math.min(1, pct + 0.3), $prev_pageview_max_content_percentage: Math.min(1, pct + 0.3) },
  });
  rows.push(scroll("s1", "1", 0.25), scroll("s2", "1", 0.55), scroll("s3", "1", 0.95), scroll("s4", "1", 1), scroll("s4", "1", 1, "$pageview"));
  await insertEvents(pg, site.project_id, rows);
  await insertEvents(pg, other.project_id, [heat("x", 1280, { [pageUrl]: [{ x: 640, y: 216, type: "click" }] })]);
});

const q = (params: Record<string, string>) => `/heatmaps?${new URLSearchParams({ url: pageUrl, ...params })}`;

test("click points match the hand count per bucket; other types, pages and projects stay out", { skip: skipNoPg }, async () => {
  const r = await api(run, site, q({}));
  assert.equal(r.status, 200, JSON.stringify(r.body));
  const got = new Map<string, number>((r.body.points as { x: number; y: number; count: number }[]).map((p) => [`${p.x}|${p.y}`, p.count]));
  assert.deepEqual([...got.entries()].sort(), [...hand.entries()].sort());
  assert.equal(r.body.total, 120);
  const rage = await api(run, site, q({ type: "rageclick" }));
  assert.equal(rage.body.total, 6);
  const moves = await api(run, site, q({ type: "mousemove" }));
  assert.equal(moves.body.total, 60);
  const phones = await api(run, site, q({ viewport_max: "500" }));
  assert.equal(phones.body.total, 40);
  const wide = await api(run, site, q({ viewport_min: "1300" }));
  assert.equal(wide.body.total, 40);
  const tenDays = await api(run, site, q({ from: ago(60 * 24 * 11).toISOString() }));
  assert.equal(tenDays.body.total, 121, "a wider range includes the older click");
  const theirs = await api(run, other, q({}));
  assert.equal(theirs.body.total, 1);
  assert.equal((await api(run, site, q({ type: "hover" }))).status, 400);
  assert.equal((await api(run, site, "/heatmaps?url=javascript:alert(1)")).status, 400);
  const urls = await api(run, site, "/heatmaps/urls");
  // 60 views x 3 points, 6 rage clicks, the older click and the damaged row (its point still counts as sent).
  assert.deepEqual(urls.body.urls[0], { url: pageUrl, clicks: 188 });
});

test("scroll depth counts each page view once", { skip: skipNoPg }, async () => {
  const r = await api(run, site, q({}));
  const s = r.body.scroll;
  assert.equal(s.views, 4);
  const reached = Object.fromEntries(s.bands.map((b: { percent: number; reached: number }) => [b.percent, b.reached]));
  assert.deepEqual(reached, { 10: 4, 20: 4, 30: 3, 40: 3, 50: 3, 60: 2, 70: 2, 80: 2, 90: 2, 100: 1 });
  assert.equal(s.page_height, 1200);
});

test("collection turns on through the app's config", { skip: skipNoPg }, async () => {
  const cfg = async () => ((await (await fetch(`${run.base}/array/${site.token}/config`)).json()) as { heatmaps: boolean }).heatmaps;
  assert.equal(await cfg(), false);
  assert.equal((await api(run, site, "/recording-settings", { method: "PUT", body: { heatmaps_enabled: true } })).status, 200);
  assert.equal(await cfg(), true);
});

test("the overlay opens from a signed link and draws the points over the page in Chromium", { skip: skipNoPg || (existsSync(chromium.executablePath()) ? false : "no cached Chromium") }, async () => {
  const link = await api(run, site, "/heatmaps/link", { body: { url: pageUrl, width: 1280 } });
  assert.equal(link.status, 200, JSON.stringify(link.body));
  const page = await fetch(link.body.url);
  assert.equal(page.status, 200);
  assert.match(page.headers.get("content-security-policy") ?? "", /frame-src/);
  const bad = new URL(link.body.url);
  bad.searchParams.set("p", other.project_id);
  assert.equal((await fetch(bad)).status, 403);
  const browser = await chromium.launch({ headless: true, channel: "chromium" });
  try {
    const p = await browser.newPage({ viewport: { width: 1400, height: 1000 } });
    const errors: string[] = [];
    p.on("pageerror", (e) => errors.push(e.message));
    await p.goto(link.body.url);
    await p.waitForFunction(() => (window as unknown as { __heatmap?: { ready: boolean } }).__heatmap?.ready === true, undefined, { timeout: 15_000 });
    const state = await p.evaluate(() => (window as unknown as { __heatmap: { points: number; total: number; error: string | null } }).__heatmap);
    assert.equal(state.error, null);
    assert.equal(state.total, 120);
    const painted = await p.evaluate(() => {
      const c = document.getElementById("heat") as HTMLCanvasElement;
      const d = c.getContext("2d")!.getImageData(0, 0, c.width, c.height).data;
      let n = 0;
      for (let i = 3; i < d.length; i += 4) if (d[i] > 0) n += 1;
      return n;
    });
    assert.ok(painted > 1000, `heat drawn on ${painted} pixels`);
    assert.equal(await p.frameLocator("iframe").locator("h1").textContent(), "Pricing", "the page shows under the map");
    assert.equal(await p.locator(".scroll-band").count(), 10);
    assert.deepEqual(errors, []);
  } finally {
    await browser.close();
  }
});
