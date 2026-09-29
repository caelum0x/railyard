// @railyard/openfeature (Wave P PY2.1): the polling client, offline start, the
// OpenFeature providers, and the 60 s promise (poll 30 s + read cache 15 s)
// on a fake clock and a fake read endpoint that caches like the real one.
//   node --test test/*.test.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { createFlagsClient, RailyardFlagsProvider, RailyardServerFlagsProvider } from "../src/index.mjs";

const KEY = `ry_flags_${"k".repeat(32)}`;

/** A fake clock with timers the client schedules on. */
function clock() {
  let now = 0;
  let timers = [];
  let id = 0;
  return {
    now: () => now,
    setTimeout: (fn, ms) => {
      timers.push({ id: ++id, at: now + ms, fn });
      return id;
    },
    clearTimeout: (t) => {
      timers = timers.filter((x) => x.id !== t);
    },
    async advance(ms) {
      const end = now + ms;
      for (;;) {
        timers.sort((a, b) => a.at - b.at);
        const next = timers[0];
        if (!next || next.at > end) break;
        timers = timers.slice(1);
        now = next.at;
        await next.fn();
        await new Promise((r) => setImmediate(r));
      }
      now = end;
    },
  };
}

/** The read endpoint: flags per key, cached `cacheMs` per URL like the edge cache. */
function server(c, cacheMs = 15_000) {
  const state = { flags: { new_checkout: { enabled: false, value: "blue" } }, reads: 0, down: false };
  let cached = null;
  const fetchImpl = async (url) => {
    state.reads += 1;
    assert.ok(String(url).includes(`key=${KEY}`));
    if (state.down) throw new Error("offline");
    if (!cached || c.now() - cached.at >= cacheMs) cached = { at: c.now(), body: JSON.stringify({ env: "production", flags: state.flags }) };
    return new Response(cached.body, { status: 200 });
  };
  return { state, fetchImpl };
}

test("a flip reaches the app within 60 s with no redeploy, even in the worst cache timing", async () => {
  const c = clock();
  const s = server(c);
  const client = createFlagsClient({ key: KEY, fetch: s.fetchImpl, setTimeout: c.setTimeout, clearTimeout: c.clearTimeout });
  await client.ready();
  assert.equal(client.isEnabled("new_checkout"), false);
  assert.equal(client.getValue("new_checkout"), "blue");
  // Flip right after the app's read filled the cache: the worst case.
  await c.advance(29_999);
  s.state.flags = { new_checkout: { enabled: true, value: "blue" } };
  const flippedAt = c.now();
  let seenAt = null;
  client.subscribe(() => (seenAt ??= c.now()));
  await c.advance(60_000);
  assert.ok(seenAt !== null, "the flip arrived");
  assert.ok(seenAt - flippedAt <= 60_000, `arrived after ${seenAt - flippedAt} ms`);
  assert.equal(client.isEnabled("new_checkout"), true);
  client.close();
});

test("an app that starts offline uses its last flags; errors back off and keep the last good answer", async () => {
  const store = new Map();
  const storage = { getItem: async (k) => store.get(k) ?? null, setItem: async (k, v) => void store.set(k, v) };
  const c = clock();
  const s = server(c, 0);
  const first = createFlagsClient({ key: KEY, fetch: s.fetchImpl, storage, setTimeout: c.setTimeout, clearTimeout: c.clearTimeout });
  await first.ready();
  first.close();
  s.state.down = true;
  const second = createFlagsClient({ key: KEY, fetch: s.fetchImpl, storage, setTimeout: c.setTimeout, clearTimeout: c.clearTimeout });
  assert.equal(await second.ready(), true);
  assert.equal(second.source(), "cache");
  assert.equal(second.getValue("new_checkout"), "blue");
  const readsBefore = s.state.reads;
  await c.advance(30_000 * 2 + 1);
  assert.equal(s.state.reads - readsBefore, 1, "after a failure the next read waits twice as long");
  s.state.down = false;
  await c.advance(30_000 * 4);
  assert.equal(second.source(), "live");
  second.close();
});

test("unknown flags fall back; a bad key is refused before any request; close stops polling", async () => {
  assert.throws(() => createFlagsClient({ key: "nope" }), /flags key/);
  const c = clock();
  const s = server(c);
  const client = createFlagsClient({ key: KEY, fetch: s.fetchImpl, setTimeout: c.setTimeout, clearTimeout: c.clearTimeout, pollMs: 1 });
  await client.ready();
  assert.equal(client.isEnabled("missing", true), true);
  assert.equal(client.getValue("missing", "x"), "x");
  client.close();
  const reads = s.state.reads;
  await c.advance(120_000);
  assert.equal(s.state.reads, reads);
});

test("OpenFeature client provider: resolves every type and emits configuration changes", async () => {
  const c = clock();
  const s = server(c, 0);
  s.state.flags = { on: { enabled: true, value: "7" }, json: { enabled: true, value: '{"a":1}' }, off: { enabled: false, value: "x" } };
  const events = [];
  const provider = new RailyardFlagsProvider({ key: KEY, fetch: s.fetchImpl, setTimeout: c.setTimeout, clearTimeout: c.clearTimeout, events: { emit: (e) => events.push(e) } });
  await provider.initialize();
  assert.deepEqual(provider.resolveBooleanEvaluation("on", false), { value: true, reason: "STATIC", variant: "on" });
  assert.equal(provider.resolveNumberEvaluation("on", 0).value, 7);
  assert.deepEqual(provider.resolveObjectEvaluation("json", {}).value, { a: 1 });
  assert.equal(provider.resolveStringEvaluation("off", "fallback").value, "fallback");
  assert.equal(provider.resolveBooleanEvaluation("missing", true).errorCode, "FLAG_NOT_FOUND");
  assert.equal(provider.resolveNumberEvaluation("json", 3).errorCode, "TYPE_MISMATCH");
  s.state.flags = { ...s.state.flags, on: { enabled: false, value: "7" } };
  await c.advance(30_000);
  assert.deepEqual(events, ["PROVIDER_CONFIGURATION_CHANGED", "PROVIDER_CONFIGURATION_CHANGED"]);
  assert.equal(provider.resolveBooleanEvaluation("on", true).value, false);
  await provider.onClose();
});

test("OpenFeature server provider (Next.js): reads on demand, cached 15 s, keeps the last answer on errors", async () => {
  let now = 0;
  const c = { now: () => now };
  const s = server(c, 0);
  const provider = new RailyardServerFlagsProvider({ key: KEY, fetch: s.fetchImpl, now: () => now });
  assert.equal((await provider.resolveBooleanEvaluation("new_checkout", true)).value, false);
  s.state.flags = { new_checkout: { enabled: true, value: null } };
  now = 10_000;
  assert.equal((await provider.resolveBooleanEvaluation("new_checkout", true)).value, false, "inside the 15 s window");
  now = 16_000;
  assert.equal((await provider.resolveBooleanEvaluation("new_checkout", false)).value, true);
  s.state.down = true;
  now = 40_000;
  assert.equal((await provider.resolveBooleanEvaluation("new_checkout", false)).value, true, "last answer kept");
  assert.throws(() => new RailyardServerFlagsProvider({ key: "bad" }), /flags key/);
});
