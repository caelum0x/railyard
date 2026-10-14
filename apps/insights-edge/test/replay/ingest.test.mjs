// Session replay ingestion (POST /api/replay) and the public recording
// config it is paired with (GET /api/replay/config). Every cap (paid gate,
// DNT/GPC, bot UA, byte cap, monthly session cap, hostname mismatch) is
// enforced server-side, not just trusted from the client.
import assert from "node:assert/strict";
import { test } from "node:test";
import { startCollector } from "../harness.mjs";

const PAID_ON = {
  org_id: "org-1",
  app_id: "shop",
  label: "shop.example",
  hostname: "shop.example",
  events_limit: 1_000_000,
  paid: true,
  // sessions_per_month and max_bytes are both deliberately below resolveReplayConfig's floors (10 and 200,000),
  // to prove those floors — not this fixture — are what actually gets enforced.
  replay: { enabled: true, sample_rate: 1, sessions_per_month: 3, max_bytes: 2000, max_session_ms: 1_800_000 },
};
const UNPAID_ENABLED = { ...PAID_ON, paid: false };
const PAID_DISABLED = { ...PAID_ON, replay: { ...PAID_ON.replay, enabled: false } };

const bindings = { INGEST_URL: "", STATS_TOKEN: "stats-token-123" };

const REAL_UA = "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_0) AppleWebKit/605.1.15 Safari/605.1.15";
const chunk = (site, overrides = {}) => ({ site, session: "sess00000000000001", seq: 0, url: "/pricing", hostname: "shop.example", events: [{ type: 3, data: {}, timestamp: Date.now() }], ...overrides });
const post = (c, body, headers = {}) => fetch(`${c.base}/api/replay`, { method: "POST", headers: { "content-type": "text/plain", "user-agent": REAL_UA, ...headers }, body: JSON.stringify(body) });

test("an unpaid site is told to stop and nothing is written, even though replay.enabled is true in storage", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings, sites: { s_unpaid: UNPAID_ENABLED } });
  try {
    const res = await post(c, chunk("s_unpaid"));
    assert.equal(res.status, 202);
    const body = await res.json();
    assert.equal(body.stop, true);
    const bucket = await c.mf.getR2Bucket("REPLAY");
    assert.equal((await bucket.list({ prefix: "s_unpaid/" })).objects.length, 0);
  } finally {
    await c.close();
  }
});

test("a paid site with replay turned off is told to stop and nothing is written", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings, sites: { s_off: PAID_DISABLED } });
  try {
    const res = await post(c, chunk("s_off"));
    assert.equal((await res.json()).stop, true);
    const bucket = await c.mf.getR2Bucket("REPLAY");
    assert.equal((await bucket.list({ prefix: "s_off/" })).objects.length, 0);
  } finally {
    await c.close();
  }
});

test("a paid, enabled site stores the chunk in R2 and a session row appears in D1", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings, sites: { s_ok: PAID_ON } });
  try {
    const res = await post(c, chunk("s_ok"));
    assert.equal(res.status, 202);
    assert.deepEqual(await res.json(), { ok: true, stop: false });
    const bucket = await c.mf.getR2Bucket("REPLAY");
    const listed = await bucket.list({ prefix: "s_ok/sess00000000000001/" });
    assert.equal(listed.objects.length, 1);
    const stored = JSON.parse(await (await bucket.get(listed.objects[0].key)).text());
    assert.equal(stored.length, 1);

    const db = await c.mf.getD1Database("DB");
    const row = await db.prepare("SELECT * FROM replay_sessions WHERE site_key = ? AND session_id = ?").bind("s_ok", "sess00000000000001").first();
    assert.equal(row.event_count, 1);
    assert.equal(row.chunk_count, 1);
    assert.equal(row.ended, 0);
  } finally {
    await c.close();
  }
});

test("Do Not Track and Global Privacy Control are both respected and nothing is stored", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings, sites: { s_ok: PAID_ON } });
  try {
    const dnt = await post(c, chunk("s_ok"), { dnt: "1" });
    assert.equal(dnt.status, 202);
    assert.equal(dnt.headers.get("x-railyard-dropped"), "dnt");
    const gpc = await post(c, chunk("s_ok", { session: "sess00000000000002" }), { "sec-gpc": "1" });
    assert.equal(gpc.headers.get("x-railyard-dropped"), "gpc");
    const bucket = await c.mf.getR2Bucket("REPLAY");
    assert.equal((await bucket.list({ prefix: "s_ok/" })).objects.length, 0);
  } finally {
    await c.close();
  }
});

test("a bot user agent is dropped before anything is stored", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings, sites: { s_ok: PAID_ON } });
  try {
    const res = await post(c, chunk("s_ok"), { "user-agent": "Googlebot/2.1" });
    assert.equal(res.headers.get("x-railyard-dropped"), "bot");
    const bucket = await c.mf.getR2Bucket("REPLAY");
    assert.equal((await bucket.list({ prefix: "s_ok/" })).objects.length, 0);
  } finally {
    await c.close();
  }
});

test("an unknown site key is refused", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings, sites: {} });
  try {
    assert.equal((await post(c, chunk("s_missing"))).status, 404);
  } finally {
    await c.close();
  }
});

test("a hostname that is not the site's own domain is refused", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings, sites: { s_ok: PAID_ON } });
  try {
    const res = await post(c, chunk("s_ok", { hostname: "evil.example" }));
    assert.equal(res.status, 403);
  } finally {
    await c.close();
  }
});

test("the per-session byte cap (floored at 200,000 bytes regardless of a site's own setting) truncates the session and tells the client to stop, without storing the over-cap chunk", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings, sites: { s_ok: PAID_ON } });
  try {
    // PAID_ON asks for a 2,000-byte cap, but resolveReplayConfig floors it at 200,000. One ~157 KB chunk
    // stays under that floor on its own; a second, equally sized chunk pushes the cumulative total over it.
    const bigEvents = () => Array.from({ length: 150 }, () => ({ type: 3, data: { x: "y".repeat(1000) }, timestamp: Date.now() }));
    const first = await post(c, chunk("s_ok", { events: bigEvents() }));
    assert.equal(first.status, 202);
    assert.equal((await first.json()).stop, false);
    const second = await post(c, chunk("s_ok", { seq: 1, events: bigEvents() }));
    const body = await second.json();
    assert.equal(body.stop, true);
    const bucket = await c.mf.getR2Bucket("REPLAY");
    // Only the first (under-cap) chunk was ever written; the over-cap chunk is not.
    assert.equal((await bucket.list({ prefix: "s_ok/sess00000000000001/" })).objects.length, 1);
    const db = await c.mf.getD1Database("DB");
    const row = await db.prepare("SELECT truncated FROM replay_sessions WHERE site_key = ? AND session_id = ?").bind("s_ok", "sess00000000000001").first();
    assert.equal(row.truncated, 1);
  } finally {
    await c.close();
  }
});

test("the monthly session cap stops a brand-new session once the limit is reached, without affecting sessions already in progress", async () => {
  // PAID_ON asks for a cap of 3, but resolveReplayConfig floors sessions_per_month at 10 — the real, enforced cap.
  const c = await startCollector({ d1: true, r2: true, bindings, sites: { s_ok: PAID_ON } });
  try {
    const ids = Array.from({ length: 10 }, (_, i) => `sess_ok_${String(i).padStart(3, "0")}`);
    for (const id of ids) {
      const res = await post(c, chunk("s_ok", { session: id, seq: 0 }));
      assert.equal((await res.json()).stop, false, `${id} should be allowed under the cap`);
    }
    // An 11th brand-new session is over the cap.
    const over = await post(c, chunk("s_ok", { session: "sess_over_11", seq: 0 }));
    assert.equal((await over.json()).stop, true);
    const bucket = await c.mf.getR2Bucket("REPLAY");
    assert.equal((await bucket.list({ prefix: "s_ok/sess_over_11/" })).objects.length, 0);
    // A later chunk of an already-admitted session keeps working even though the site is now "at the cap".
    const continued = await post(c, chunk("s_ok", { session: ids[0], seq: 1 }));
    assert.equal((await continued.json()).stop, false);
  } finally {
    await c.close();
  }
});

test("GET /api/replay/config reflects the site's own settings, degrades to disabled once unpaid, and 404s for an unknown site", async () => {
  const c = await startCollector({ d1: true, r2: true, bindings, sites: { s_ok: PAID_ON, s_unpaid: UNPAID_ENABLED } });
  try {
    const cfg = await (await fetch(`${c.base}/api/replay/config?site=s_ok`)).json();
    assert.equal(cfg.enabled, true);
    assert.equal(cfg.sample_rate, 1);
    assert.equal(cfg.mask_all_inputs, true);
    assert.equal(cfg.mask_text_selector, "*");
    assert.equal("sessions_per_month" in cfg, false, "the public config never leaks the session cap or any private knob");

    const unpaid = await (await fetch(`${c.base}/api/replay/config?site=s_unpaid`)).json();
    assert.equal(unpaid.enabled, false, "enabled in storage but unpaid still reports disabled to the tag");

    assert.equal((await fetch(`${c.base}/api/replay/config?site=s_missing`)).status, 404);
  } finally {
    await c.close();
  }
});
