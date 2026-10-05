// QY1.2 (A18): the web view over product analytics events matches a hand
// count (visitors, views, sessions, bounce rate, top pages, entry referrers,
// entry UTM, page speed p75 with its rating); another project's rows never
// show; page-tag events forwarded by the edge collector (its exact shape)
// land and count; the server-side door takes only the project's server key.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { flushed, makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { api, insertEvents, SECRET, token, type Row } from "../a3/helpers.ts";
import { serverKeyFor, verifyServerKey } from "../../src/web/server-key.ts";
// The edge collector's mapping, imported as is: the two sides cannot drift.
import { toPaEvent } from "../../../insights-edge/src/pa-forward.mjs";

let pg: ScratchPg;
let run: Running;
const site = makeSite({ name: "shop" });
const other = makeSite({ name: "other" });
const tagSite = makeSite({ name: "tag" });
const now = new Date("2026-10-05T12:00:00Z");
const at = (hoursAgo: number) => new Date(now.getTime() - hoursAgo * 3_600_000);

const pv = (distinct: string, session: string, hoursAgo: number, path: string, extra: Record<string, unknown> = {}): Row => ({
  event: "$pageview",
  distinct_id: distinct,
  session_id: session,
  timestamp: at(hoursAgo),
  properties: { $current_url: `https://shop.example${path}`, $pathname: path.split("?")[0], $host: "shop.example", ...extra },
});
const vit = (distinct: string, hoursAgo: number, v: Record<string, number>): Row => ({
  event: "$web_vitals",
  distinct_id: distinct,
  timestamp: at(hoursAgo),
  properties: Object.fromEntries(Object.entries(v).map(([k, n]) => [`$web_vitals_${k}_value`, n])),
});

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site, other, tagSite], { consoleSecret: SECRET, now: () => now });
  await insertEvents(pg, site.project_id, [
    // ann: one session from a newsletter, three pages
    pv("ann", "s1", 30, "/?utm_source=newsletter&utm_medium=email&utm_campaign=launch", { $referring_domain: "mail.example.org", utm_source: "newsletter", utm_medium: "email", utm_campaign: "launch" }),
    pv("ann", "s1", 29.9, "/pricing", { $referring_domain: "shop.example" }),
    pv("ann", "s1", 29.8, "/signup", { $referring_domain: "shop.example" }),
    // bob: two sessions, a search then direct, one page each (two bounces)
    pv("bob", "s2", 20, "/pricing", { $referring_domain: "search.example.com" }),
    pv("bob", "s3", 5, "/", {}),
    // cid: one session from search, two pages
    pv("cid", "s4", 2, "/blog/a", { $referring_domain: "search.example.com", utm_source: "ads", utm_medium: "cpc" }),
    pv("cid", "s4", 1.9, "/pricing", { $referring_domain: "shop.example" }),
    // outside the range (8 days ago) and a non-page event: never counted
    pv("old", "s0", 8 * 24 + 1, "/old", {}),
    { event: "signup", distinct_id: "ann", timestamp: at(29.7), properties: { $pathname: "/signup" } },
    vit("ann", 29.9, { LCP: 1200, INP: 80, CLS: 0.01, FCP: 700 }),
    vit("bob", 20, { LCP: 2600, INP: 240, CLS: 0.3 }),
    vit("bob", 5, { LCP: 3100, INP: 120, CLS: 0.02 }),
    vit("cid", 2, { LCP: 5200, INP: 600, CLS: 0.05 }),
  ]);
  // Another project with many page views of its own.
  await insertEvents(pg, other.project_id, Array.from({ length: 40 }, (_, i) => pv(`o${i}`, `o${i}`, 3, "/pricing", { $referring_domain: "leak.example", utm_source: "leak" })).concat([vit("o1", 3, { LCP: 99_000 })]));
});

test("totals, series, pages, entry referrers and entry UTM match the hand count", { skip: skipNoPg }, async () => {
  const r = await api(run, site, "/web?days=7", { now });
  assert.equal(r.status, 200, JSON.stringify(r.body));
  const w = r.body;
  assert.deepEqual(w.totals, { visitors: 3, views: 7, sessions: 4, bounce_rate: 0.5 });
  assert.equal(w.series.length, 8, "seven days from noon touch eight UTC days");
  assert.equal(w.series.reduce((a: number, d: { views: number }) => a + d.views, 0), 7);
  assert.deepEqual(w.pages[0], { value: "/pricing", views: 3, visitors: 3 });
  assert.deepEqual(
    w.pages.map((p: { value: string }) => p.value),
    ["/pricing", "/", "/blog/a", "/signup"],
  );
  // Entry referrers: one per session; a self-referral is direct.
  assert.deepEqual(w.referrers, [
    { value: "search.example.com", views: 2, visitors: 2 },
    { value: "$direct", views: 1, visitors: 1 },
    { value: "mail.example.org", views: 1, visitors: 1 },
  ]);
  assert.deepEqual(w.utm.source, [
    { value: "ads", views: 1, visitors: 1 },
    { value: "newsletter", views: 1, visitors: 1 },
  ]);
  assert.deepEqual(w.utm.campaign, [{ value: "launch", views: 1, visitors: 1 }]);
});

test("page speed: p75 per metric with its rating, from the same store", { skip: skipNoPg }, async () => {
  const w = (await api(run, site, "/web?days=7", { now })).body;
  const p75 = (xs: number[]) => {
    const s = [...xs].sort((a, b) => a - b);
    const pos = 0.75 * (s.length - 1);
    const lo = Math.floor(pos);
    return s[lo]! + (s[Math.min(lo + 1, s.length - 1)]! - s[lo]!) * (pos - lo);
  };
  assert.equal(w.vitals.LCP.p75, p75([1200, 2600, 3100, 5200]));
  assert.equal(w.vitals.LCP.samples, 4);
  assert.equal(w.vitals.LCP.rating, "needs_improvement");
  assert.equal(w.vitals.INP.p75, p75([80, 240, 120, 600]));
  assert.equal(w.vitals.CLS.rating, "needs_improvement");
  assert.deepEqual(w.vitals.FCP, { p75: 700, samples: 1, rating: "good" });
  assert.deepEqual(w.vitals.TTFB, { p75: null, samples: 0, rating: null });
});

test("another project's rows never show, and its token cannot read this project", { skip: skipNoPg }, async () => {
  const w = (await api(run, site, "/web?days=7", { now })).body;
  assert.ok(!JSON.stringify(w).includes("leak"));
  const o = (await api(run, other, "/web?days=7", { now })).body;
  assert.equal(o.totals.views, 40);
  const cross = await fetch(`${run.base}/v1/projects/${site.project_id}/web`, {
    headers: { authorization: `Bearer ${await token(other, "owner", now)}` },
  });
  assert.equal(cross.status, 403);
});

test("bad ranges are refused with a plain message", { skip: skipNoPg }, async () => {
  assert.equal((await api(run, site, "/web?days=200", { now })).status, 400);
  assert.equal((await api(run, site, "/web?limit=0", { now })).status, 400);
  assert.equal((await api(run, site, `/web?from=${now.toISOString()}&to=${at(1).toISOString()}`, { now })).status, 400);
});

test("page-tag events in the edge collector's shape land through /batch and show in the web view", { skip: skipNoPg }, async () => {
  const base = { visitor: "c0ffee".repeat(5) + "ab", ts: at(1).toISOString(), hostname: "tag.example", country: "NL" };
  const events = [
    toPaEvent({ ...base, type: "event", url: "/landing?utm_source=launchpost", referrer: "https://forum.example.net/t/1" }, crypto.randomUUID()),
    toPaEvent({ ...base, type: "performance", url: "/landing", lcp: 1500, inp: 90, cls: 0.02 }, crypto.randomUUID()),
  ];
  const res = await fetch(`${run.base}/batch`, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ api_key: tagSite.token, sent_at: now.toISOString(), batch: events }) });
  assert.equal(res.status, 200, await res.text());
  await flushed(run);
  const w = (await api(run, tagSite, "/web?days=1", { now })).body;
  assert.equal(w.totals.views, 1);
  assert.deepEqual(w.pages, [{ value: "/landing", views: 1, visitors: 1 }]);
  assert.deepEqual(w.referrers, [{ value: "forum.example.net", views: 1, visitors: 1 }]);
  assert.deepEqual(w.utm.source, [{ value: "launchpost", views: 1, visitors: 1 }]);
  assert.deepEqual(w.vitals.LCP, { p75: 1500, samples: 1, rating: "good" });
  const persons = Number(pg.psql(`select count(*) from pa.persons where project_id = '${tagSite.project_id}'`));
  assert.equal(persons, 0, "a cookieless visitor gets no person profile");
});

test("server-side capture takes only this project's server key", { skip: skipNoPg }, async () => {
  const key = serverKeyFor(SECRET, site.project_id);
  assert.match(key, /^ry_pak_[0-9a-f]{64}$/);
  assert.ok(verifyServerKey(SECRET, site.project_id, key));
  assert.ok(!verifyServerKey(SECRET, other.project_id, key));
  const body = (event: string, extra: Record<string, unknown> = {}) =>
    JSON.stringify({ batch: [{ event, distinct_id: "user_9", uuid: crypto.randomUUID(), properties: { amount: 9.99 } }], ...extra });
  const send = (id: string, auth: string | null, b: string) =>
    fetch(`${run.base}/v1/server/${id}/batch`, { method: "POST", headers: { "content-type": "application/json", ...(auth ? { authorization: `Bearer ${auth}` } : {}) }, body: b });
  assert.equal((await send(site.project_id, key, body("purchase_completed"))).status, 200);
  assert.equal((await send(other.project_id, key, body("purchase_completed"))).status, 401, "another project's key");
  assert.equal((await send(site.project_id, site.token, body("purchase_completed"))).status, 401, "the public site key is not a server key");
  assert.equal((await send(site.project_id, null, body("purchase_completed"))).status, 401);
  assert.equal((await send(site.project_id, key, body("purchase_completed", { api_key: other.token }))).status, 400, "a payload naming another project");
  await flushed(run);
  assert.equal(Number(pg.psql(`select count(*) from pa.events where project_id = '${site.project_id}' and event = 'purchase_completed'`)), 1);
  assert.equal(Number(pg.psql(`select count(*) from pa.events where project_id = '${other.project_id}' and event = 'purchase_completed'`)), 0);
});
