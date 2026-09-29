import { test } from "node:test";
import assert from "node:assert/strict";
import { dailySalt, visitorHash } from "../src/visitor.mjs";
import { dropReason } from "../src/bots.mjs";
import { parseOne } from "../src/payload.mjs";

test("the visitor hash changes by day and by site, never contains the IP", async () => {
  const d1 = await dailySalt("s", Date.parse("2026-09-29T10:00:00Z"));
  const d1b = await dailySalt("s", Date.parse("2026-09-29T23:59:00Z"));
  const d2 = await dailySalt("s", Date.parse("2026-09-30T00:00:01Z"));
  assert.equal(d1, d1b);
  assert.notEqual(d1, d2);
  const a = await visitorHash({ salt: d1, siteKey: "site1", ip: "198.51.100.4", userAgent: "UA" });
  assert.notEqual(a, await visitorHash({ salt: d2, siteKey: "site1", ip: "198.51.100.4", userAgent: "UA" }));
  assert.notEqual(a, await visitorHash({ salt: d1, siteKey: "site2", ip: "198.51.100.4", userAgent: "UA" }));
  assert.ok(!a.includes("198"));
});

test("real browsers pass; bots, tools and prefetches drop", () => {
  const h = (o) => new Headers(o);
  assert.equal(dropReason(h({ "user-agent": "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148 Safari/604.1" })), null);
  for (const ua of ["curl/8.5.0", "python-requests/2.32", "Mozilla/5.0 (compatible; bingbot/2.0)", "Mozilla/5.0 HeadlessChrome/120", "GPTBot/1.1", ""]) assert.equal(dropReason(h({ "user-agent": ua })), "bot", ua);
  assert.equal(dropReason(h({ "user-agent": "Mozilla/5.0 Safari/605", purpose: "prefetch" })), "prefetch");
});

test("payload keeps listed fields only, clamps lengths", () => {
  const p = parseOne({ type: "event", payload: { website: "ry_site_abcdefgh", url: "/x", ip: "1.2.3.4", name: "signup", data: { plan: "pro", n: 2, nested: { a: 1 } }, title: "t".repeat(900) } });
  assert.deepEqual(Object.keys(p.event).sort(), ["data", "hostname", "language", "name", "referrer", "screen", "title", "type", "url"]);
  assert.deepEqual(p.event.data, { plan: "pro", n: 2 });
  assert.equal(p.event.title.length, 500);
  assert.equal(parseOne({ type: "hack", payload: {} }).error, "invalid type");
});
