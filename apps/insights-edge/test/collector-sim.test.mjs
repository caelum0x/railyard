// The OrgCollector logic under 1,000 events a second for 60 simulated seconds
// (fake clock and storage, in Node): every event is answered, every stored
// event is forwarded once, each site gets at most one ingest request per
// second, and the cap holds — pooled org-wide across the org's three sites
// (every site of a real org carries the same plan events_limit, copied from
// the org's single entitlement by apps/console/lib/sites.ts, so the cap is
// one shared budget, not three independent ones). The Miniflare load run
// (test/load.mjs) covers the same path in workerd; its numbers are in
// LOAD-RESULT.md.
import { test } from "node:test";
import assert from "node:assert/strict";
import { OrgCollector, FLUSH_MS } from "../src/collector.mjs";

function fakeState() {
  const data = new Map();
  let alarm = null;
  return {
    storage: {
      get: async (k) => data.get(k),
      put: async (k, v) => void data.set(k, v),
      getAlarm: async () => alarm,
      setAlarm: async (t) => void (alarm = t),
    },
    takeAlarm() {
      const a = alarm;
      alarm = null;
      return a;
    },
  };
}

test("1,000 events/s for 60 s: all answered, all forwarded once, <= 1 ingest request/s per site, cap enforced", async () => {
  let now = Date.parse("2026-09-29T12:00:00Z");
  const calls = [];
  const realFetch = globalThis.fetch;
  globalThis.fetch = async (url, init) => {
    calls.push({ url, at: now, body: JSON.parse(init.body) });
    return new Response("{}");
  };
  try {
    const state = fakeState();
    const col = new OrgCollector(state, { INGEST_URL: "https://ingest.test", RAGO_URL: "https://rago.test" });
    col.now = () => now;
    // Real orgs carry the SAME events_limit on every site (copied from the
    // org's one entitlement) — the pool, not each site's own number, is what
    // this test exercises under load.
    const sites = [
      { key: "s1", app_id: "a1", events_limit: 15_000 },
      { key: "s2", app_id: "a2", events_limit: 15_000 },
      { key: "s3", app_id: "a3", events_limit: 15_000 },
    ];
    let answered = 0;
    let stored = 0;
    let nextAlarm = null;
    for (let ms = 0; ms < 60_000; ms += 1) {
      now = Date.parse("2026-09-29T12:00:00Z") + ms;
      const site = sites[ms % 3];
      const res = await col.fetch(new Request("https://c/ingest", { method: "POST", body: JSON.stringify({ orgId: "org-1", site, events: [{ url: `/${ms}`, visitor: "v", ts: "t" }] }) }));
      const r = await res.json();
      answered += 1;
      stored += r.stored;
      const a = state.takeAlarm();
      if (a !== null) nextAlarm = a;
      if (nextAlarm !== null && now >= nextAlarm) {
        nextAlarm = null;
        await col.alarm();
        const again = state.takeAlarm();
        if (again !== null) nextAlarm = again;
      }
    }
    now += 61_000;
    await col.alarm();
    assert.equal(answered, 60_000);
    const ingest = calls.filter((c) => c.url.includes("/v1/ingest/batch"));
    const forwarded = ingest.flatMap((c) => c.body.batches.flatMap((b) => b.events.map((e) => e.url)));
    assert.equal(forwarded.length, stored);
    assert.equal(new Set(forwarded).size, forwarded.length, "no event forwarded twice");
    assert.equal(stored, 15_000, "the org's pooled 15,000-event cap, shared across all three of its sites");
    for (const s of ["s1", "s2", "s3"]) {
      const times = ingest.filter((c) => c.body.batches.some((b) => b.site_key === s)).map((c) => c.at);
      for (let i = 1; i < times.length; i += 1) assert.ok(times[i] - times[i - 1] >= 1000, `${s}: two ingest requests within 1 s`);
      assert.ok(times.length <= 60_000 / FLUSH_MS + 2);
    }
    const usage = calls.filter((c) => c.url.endsWith("/api/v1/events")).map((c) => c.body.event.properties);
    const sum = usage.reduce((a, p) => ({ events: a.events + p.events, capped: a.capped + p.capped }), { events: 0, capped: 0 });
    assert.deepEqual(sum, { events: 60_000, capped: 45_000 }, "over-cap events are counted for Rago");
  } finally {
    globalThis.fetch = realFetch;
  }
});

test("PY2.5: a retried delivery with the same client event ids is stored once; ids expire after a day", async () => {
  let now = Date.parse("2026-09-29T12:00:00Z");
  const realFetch = globalThis.fetch;
  const forwarded = [];
  globalThis.fetch = async (url, init) => {
    if (String(url).includes("/v1/ingest/batch")) forwarded.push(...JSON.parse(init.body).batches.flatMap((b) => b.events));
    return new Response("{}");
  };
  try {
    const state = fakeState();
    const col = new OrgCollector(state, { INGEST_URL: "https://ingest.test" });
    col.now = () => now;
    const site = { key: "ry_site_dedupe000000", app_id: "app-d", events_limit: 100000 };
    const batch = Array.from({ length: 5 }, (_, i) => ({ type: "event", id: `evt_${String(i).padStart(8, "0")}`, url: `/p${i}` }));
    const first = await (await col.fetch(new Request("https://do", { method: "POST", body: JSON.stringify({ orgId: "org-d", site, events: batch }) }))).json();
    const again = await (await col.fetch(new Request("https://do", { method: "POST", body: JSON.stringify({ orgId: "org-d", site, events: [...batch, { type: "event", id: "evt_00000009", url: "/new" }] }) }))).json();
    assert.deepEqual([first.stored, first.duplicates], [5, 0]);
    assert.deepEqual([again.stored, again.duplicates], [1, 5]);
    now += FLUSH_MS;
    await col.alarm();
    assert.equal(forwarded.length, 6, "every event forwarded exactly once");
    // A fresh collector (evicted object) remembers the ids from storage.
    const col2 = new OrgCollector(state, { INGEST_URL: "https://ingest.test" });
    col2.now = () => now;
    const third = await (await col2.fetch(new Request("https://do", { method: "POST", body: JSON.stringify({ orgId: "org-d", site, events: batch }) }))).json();
    assert.equal(third.duplicates, 5);
    now += 25 * 3600 * 1000;
    const later = await (await col2.fetch(new Request("https://do", { method: "POST", body: JSON.stringify({ orgId: "org-d", site, events: batch.slice(0, 1) }) }))).json();
    assert.equal(later.stored, 1, "an id older than a day counts again");
    // Events without ids are never dropped.
    const plain = await (await col2.fetch(new Request("https://do", { method: "POST", body: JSON.stringify({ orgId: "org-d", site, events: [{ type: "event", url: "/a" }, { type: "event", url: "/a" }] }) }))).json();
    assert.equal(plain.stored, 2);
  } finally {
    globalThis.fetch = realFetch;
  }
});
