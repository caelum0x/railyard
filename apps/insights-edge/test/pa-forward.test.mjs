// QY1.2 (A18): with PRODUCT_ANALYTICS_URL set, the page tag's stored events
// also reach product analytics in its capture protocol, so the web view reads
// the same store: page views as $pageview with path, referrer and UTM,
// page speed as $web_vitals, no IP, cookieless ids, retried on a server error.
import { test, after, before } from "node:test";
import assert from "node:assert/strict";

import { ev, sleep, startCollector } from "./harness.mjs";
import { toPaEvent } from "../src/pa-forward.mjs";

const SITE = "ry_site_pafwdpafwd01";
const CAPPED = "ry_site_pafwdcapped1";
let c;
let failPa = 0;
before(async () => {
  c = await startCollector({
    bindings: { PRODUCT_ANALYTICS_URL: "https://pa.test" },
    sites: {
      [SITE]: { org_id: "org-pa", app_id: "app-pa", events_limit: 100000, paused: false },
      [CAPPED]: { org_id: "org-pc", app_id: "app-pc", events_limit: 1, paused: false },
    },
    respond: (r) => (r.url.startsWith("https://pa.test/") && failPa-- > 0 ? 503 : 200),
  });
});
after(() => c.close());

const paBatches = () => c.outbound.filter((o) => o.url === "https://pa.test/batch").map((o) => JSON.parse(o.body));
async function waitFor(fn, ms = 8000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    const v = fn();
    if (v) return v;
    await sleep(100);
  }
  return fn();
}

test("a page view and its page speed reach product analytics as $pageview and $web_vitals, keyed by the site key, with no IP", async () => {
  const r1 = await c.send(ev(SITE, "/pricing?utm_source=newsletter&utm_medium=email&utm_campaign=launch", { referrer: "https://news.example.org/post/1" }));
  assert.equal(r1.status, 202);
  const r2 = await c.send({ type: "performance", payload: { website: SITE, url: "/pricing", hostname: "shop.example", lcp: 1800, inp: 120, cls: 0.05 } });
  assert.equal(r2.status, 202);
  const batches = await waitFor(() => {
    const b = paBatches().filter((x) => x.api_key === SITE);
    return b.flatMap((x) => x.batch).length >= 2 ? b : null;
  });
  assert.ok(batches, "product analytics received the events");
  const events = batches.flatMap((b) => b.batch);
  const pv = events.find((e) => e.event === "$pageview");
  assert.equal(pv.properties.$pathname, "/pricing");
  assert.equal(pv.properties.$current_url, "https://shop.example/pricing?utm_source=newsletter&utm_medium=email&utm_campaign=launch");
  assert.equal(pv.properties.$referring_domain, "news.example.org");
  assert.deepEqual([pv.properties.utm_source, pv.properties.utm_medium, pv.properties.utm_campaign], ["newsletter", "email", "launch"]);
  assert.match(pv.distinct_id, /^anon_[0-9a-f]{16,40}$/);
  assert.equal(pv.properties.$process_person_profile, false);
  assert.match(pv.uuid, /^[0-9a-f-]{36}$/);
  const wv = events.find((e) => e.event === "$web_vitals");
  assert.deepEqual([wv.properties.$web_vitals_LCP_value, wv.properties.$web_vitals_INP_value, wv.properties.$web_vitals_CLS_value], [1800, 120, 0.05]);
  assert.equal(wv.properties.$session_id, pv.properties.$session_id, "one cookieless session per visitor and day");
  const raw = c.outbound.filter((o) => o.url.startsWith("https://pa.test/")).map((o) => o.body).join("\n");
  assert.ok(!raw.includes("203.0.113.77"), "the IP never leaves the collector");
});

test("a server error from product analytics is retried with the same event ids; Insights ingest is not held back", async () => {
  failPa = 1;
  const before = paBatches().length;
  await c.send(ev(SITE, "/retry-me"));
  const got = await waitFor(() => {
    const all = c.outbound.filter((o) => o.url === "https://pa.test/batch").slice(before);
    return all.length >= 2 ? all : null;
  }, 10_000);
  assert.ok(got, "retried at the next flush");
  const [first, second] = got.map((o) => JSON.parse(o.body).batch.find((e) => e.properties.$pathname === "/retry-me"));
  assert.equal(first.uuid, second.uuid, "the retry carries the same uuid, so it is stored once");
  assert.ok(c.outbound.some((o) => o.url.startsWith("https://ingest.test/") && o.body.includes("/retry-me")));
});

test("over the cap nothing goes to product analytics either", async () => {
  await c.send(ev(CAPPED, "/one"));
  await c.send(ev(CAPPED, "/two"));
  await sleep(3000);
  const paths = paBatches().filter((b) => b.api_key === CAPPED).flatMap((b) => b.batch.map((e) => e.properties.$pathname));
  assert.deepEqual(paths, ["/one"]);
});

test("custom events keep their name and data; identify links the cookieless id to the user", () => {
  const base = { visitor: "ab".repeat(32), ts: "2026-10-05T10:00:00.000Z", url: "/app", hostname: "shop.example", referrer: null, country: "DE" };
  const custom = toPaEvent({ ...base, type: "event", name: "signup", data: { plan: "pro", $ip: "1.2.3.4" } }, "u1");
  assert.equal(custom.event, "signup");
  assert.equal(custom.properties.plan, "pro");
  assert.equal(custom.properties.$ip, undefined, "no $-prefixed data keys pass");
  assert.equal(custom.properties.$referring_domain, "$direct");
  assert.equal(custom.properties.$geoip_country_code, "DE");
  const id = toPaEvent({ ...base, type: "identify", user: "user_42", data: { plan: "pro" } }, "u2");
  assert.equal(id.event, "$identify");
  assert.equal(id.distinct_id, "user_42");
  assert.equal(id.properties.$anon_distinct_id, `anon_${"ab".repeat(20)}`);
  assert.deepEqual(id.properties.$set, { plan: "pro" });
  assert.equal(id.properties.$process_person_profile, true);
});
