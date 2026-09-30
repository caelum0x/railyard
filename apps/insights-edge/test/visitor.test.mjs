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

test("PY3.2: identify carries the app's user id and traits; events may carry it; bad ids are refused", () => {
  const i = parseOne({ type: "identify", payload: { website: "ry_site_abcdefgh", url: "/", user: "user_42", data: { plan: "pro" } } });
  assert.equal(i.event.type, "identify");
  assert.equal(i.event.user, "user_42");
  assert.deepEqual(i.event.data, { plan: "pro" });
  assert.equal(parseOne({ type: "event", payload: { website: "ry_site_abcdefgh", url: "/", name: "x", user: "u@example.com" } }).event.user, "u@example.com");
  assert.equal(parseOne({ type: "identify", payload: { website: "ry_site_abcdefgh", url: "/" } }).error, "identify needs user");
  for (const bad of ["has space", "x".repeat(51), 42, "<script>"]) {
    assert.equal(parseOne({ type: "event", payload: { website: "ry_site_abcdefgh", url: "/", user: bad } }).error, "invalid user");
  }
});

test("PY3.2: the page tag exposes identify and sends it only for a plain user id", async () => {
  const { TRACKER_JS } = await import("../src/tracker.mjs");
  const sent = [];
  const listeners = {};
  const win = {};
  const ctx = {
    document: { currentScript: { getAttribute: () => "ry_site_abcdefgh", src: "https://c.test/s.js" }, readyState: "loading", referrer: "", title: "T" },
    location: { pathname: "/", search: "", hostname: "shop.example" },
    navigator: { doNotTrack: "0", language: "en", sendBeacon: (_u, blob) => (sent.push(blob), true) },
    screen: { width: 1, height: 1 },
    history: { pushState() {} },
    addEventListener: (n, f) => (listeners[n] = f),
    Blob: class { constructor(parts) { this.text = parts.join(""); } },
    URL,
    fetch: () => Promise.resolve(),
    window: win,
  };
  new Function(...Object.keys(ctx), TRACKER_JS)(...Object.values(ctx));
  win.railyard.identify("bad id");
  assert.equal(sent.length, 0);
  win.railyard.identify("user_42", { plan: "pro" });
  win.railyard.track("created_project");
  const bodies = sent.map((b) => JSON.parse(b.text));
  assert.deepEqual(bodies.map((b) => [b.type, b.payload.user, b.payload.name ?? null]), [["identify", "user_42", null], ["event", "user_42", "created_project"]]);
  assert.deepEqual(bodies[0].payload.data, { plan: "pro" });
});
