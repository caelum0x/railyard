// RYP-D3 acceptance in Miniflare (workerd): site-key check, bot and prefetch
// drop, no raw IP stored or forwarded, over-cap 202 with x-railyard-capped,
// counted in Rago and not stored, 2 s batching.
import { test, after, before } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

import { ev, sleep, startCollector } from "./harness.mjs";
import { dailySalt, visitorHash } from "../src/visitor.mjs";

const SITE = "ry_site_aaaaaaaaaaaa";
const CAPPED = "ry_site_capcapcapcap";
const PAUSED = "ry_site_pausedpaused";
let c;
before(async () => {
  c = await startCollector({
    bindings: { RAGO_BUCKET_MS: "1000" },
    sites: {
      [SITE]: { org_id: "org-a", app_id: "app-a", events_limit: 100000, paused: false },
      [CAPPED]: { org_id: "org-b", app_id: "app-b", events_limit: 3, paused: false },
      [PAUSED]: { org_id: "org-c", app_id: "app-c", events_limit: 100000, paused: true },
    },
  });
});
after(() => c.close());

function filesUnder(dir, out = []) {
  for (const n of readdirSync(dir)) {
    const p = join(dir, n);
    if (statSync(p).isDirectory()) filesUnder(p, out);
    else out.push(p);
  }
  return out;
}

test("tracker script is served", async () => {
  const res = await c.mf.dispatchFetch("https://insights.test/s.js");
  assert.equal(res.status, 200);
  assert.match(await res.text(), /data-site/);
});

test("a page view is accepted (202), batched and forwarded with a visitor hash and no IP", async () => {
  const start = c.outbound.length;
  for (let i = 0; i < 5; i += 1) assert.equal((await c.send(ev(SITE, `/p${i}`))).status, 202);
  await sleep(2600);
  const sent = c.outbound.slice(start).filter((o) => o.url.startsWith("https://ingest.test/v1/ingest/batch"));
  assert.equal(sent.length, 1, "five events, one downstream request");
  const body = JSON.parse(sent[0].body);
  assert.equal(sent[0].auth, "Bearer ingest-token");
  assert.equal(body.org_id, "org-a");
  const events = body.batches[0].events;
  assert.equal(events.length, 5);
  assert.match(events[0].visitor, /^[0-9a-f]{32}$/);
  assert.equal(new Set(events.map((e) => e.visitor)).size, 1, "same visitor, same day, same hash");
  // The Worker did see the client IP (the hash is built from it) and still forwarded none.
  const ua = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 Safari/605.1.15";
  const expected = await visitorHash({ salt: await dailySalt("test-salt-secret", Date.now()), siteKey: SITE, ip: "203.0.113.77", userAgent: ua });
  assert.equal(events[0].visitor, expected);
  for (const o of c.outbound) assert.ok(!o.body.includes("203.0.113.77"), "no raw IP downstream");
});

test("no raw IP is stored anywhere the collector persists", async () => {
  await sleep(2200);
  const files = filesUnder(c.persist);
  assert.ok(files.some((f) => f.includes("OrgCollector") || f.includes("/do")), `durable object storage persisted (${files.length} files)`);
  for (const f of files) assert.ok(!readFileSync(f).includes(Buffer.from("203.0.113.77")), `${f} holds the IP`);
});

test("bots and prefetches are dropped before counting", async () => {
  const start = c.outbound.length;
  const bot = await c.send(ev(SITE), { "user-agent": "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)" });
  assert.equal(bot.status, 202);
  assert.equal(bot.headers.get("x-railyard-dropped"), "bot");
  const pre = await c.send(ev(SITE), { "sec-purpose": "prefetch" });
  assert.equal(pre.headers.get("x-railyard-dropped"), "prefetch");
  await sleep(2300);
  assert.equal(c.outbound.slice(start).filter((o) => o.url.includes("/ingest/")).length, 0);
});

test("unknown site keys and bad bodies are refused", async () => {
  assert.equal((await c.send(ev("ry_site_nobodyhome"))).status, 404);
  assert.equal((await c.send("not json")).status, 400);
  assert.equal((await c.send({ type: "event", payload: { url: "/" } })).status, 400);
  assert.equal((await c.send(Array.from({ length: 101 }, () => ev(SITE)), {}, "/api/batch")).status, 400);
});

test("over the cap: 202 with x-railyard-capped, not stored, counted in Rago", async () => {
  const start = c.outbound.length;
  const r1 = await c.send(Array.from({ length: 3 }, (_, i) => ev(CAPPED, `/c${i}`)), {}, "/api/batch");
  assert.equal(r1.status, 202);
  assert.equal(r1.headers.get("x-railyard-capped"), null);
  const r2 = await c.send(ev(CAPPED, "/over"));
  assert.equal(r2.status, 202);
  assert.equal(r2.headers.get("x-railyard-capped"), "1");
  await sleep(3500);
  const out = c.outbound.slice(start);
  const stored = out.filter((o) => o.url.includes("/ingest/")).flatMap((o) => JSON.parse(o.body).batches.flatMap((b) => b.events));
  assert.equal(stored.length, 3);
  assert.ok(!stored.some((e) => e.url === "/over"));
  const usage = out.filter((o) => o.url === "https://rago.test/api/v1/events").map((o) => JSON.parse(o.body).event).filter((e) => e.external_subscription_id === "railyard-org-org-b");
  const total = usage.reduce((a, e) => ({ events: a.events + e.properties.events, capped: a.capped + e.properties.capped, stored: a.stored + e.properties.stored }), { events: 0, capped: 0, stored: 0 });
  assert.deepEqual(total, { events: 4, capped: 1, stored: 3 });
  assert.ok(usage.every((e) => e.code === "insights_events" && e.transaction_id.startsWith("insights:org-b:")));
});

test("a paused site answers 202 with x-railyard-paused and stores nothing", async () => {
  const start = c.outbound.length;
  const r = await c.send(ev(PAUSED));
  assert.equal(r.status, 202);
  assert.equal(r.headers.get("x-railyard-paused"), "1");
  await sleep(2300);
  assert.ok(!c.outbound.slice(start).some((o) => o.body.includes("org-c")));
});

test("the collector fails closed without a salt secret", async () => {
  const bare = await startCollector({ bindings: { VISITOR_SALT_SECRET: "" }, sites: { [SITE]: { org_id: "o", app_id: "a", events_limit: 10 } } });
  try {
    assert.equal((await bare.send(ev(SITE))).status, 503);
  } finally {
    await bare.close();
  }
});
