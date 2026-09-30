// @railyard/insights-react-native (Wave P PY3.2): screens, events, identify
// that survives a restart, reset, consent, and the offline queue sending each
// event exactly once. Payloads are checked against the edge collector's own
// parser, so the client and the collector agree.
//   node --test test/*.test.ts
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { createInsights, type HttpFetch } from '../src/index.ts';
import { parseOne } from '../../../apps/insights-edge/src/payload.mjs';

const SITE = 'ry_site_abcdefgh1234';

function memoryStorage() {
  const m = new Map<string, string>();
  return { m, getItem: async (k: string) => m.get(k) ?? null, setItem: async (k: string, v: string) => void m.set(k, v) };
}

function collector() {
  const received: { type: string; payload: Record<string, unknown> }[] = [];
  let online = true;
  const fetch: HttpFetch = async (url, init) => {
    assert.equal(url, 'https://collect.test/api/batch');
    assert.match(init?.headers?.['user-agent'] ?? '', /^Mozilla\/5\.0 \(compatible; RailyardInsightsRN/);
    if (!online) throw new Error('offline');
    const items = JSON.parse(init?.body ?? '[]');
    for (const it of items) {
      const p = parseOne(it);
      assert.ok(!('error' in p), JSON.stringify(p));
      // The collector drops a client id it has seen (PY2.5); model that here.
      if (!received.some((r) => r.payload.id === it.payload.id)) received.push(it);
    }
    return { status: 202, ok: true, json: async () => ({}) };
  };
  return { received, fetch, setOnline: (v: boolean) => (online = v) };
}

const later = () => {
  const timers: (() => void)[] = [];
  return { timers, setTimeout: (fn: () => void) => void timers.push(fn) };
};

test('screens and events reach the collector in its own format; identify tags every later event', async () => {
  const c = collector();
  const s = memoryStorage();
  const t = later();
  const i = createInsights({ site: SITE, collector: 'https://collect.test/', storage: s, fetch: c.fetch, app: 'com.acme.shop', language: 'en-US', screenSize: '390x844', setTimeout: t.setTimeout });
  await i.screen('home');
  await i.identify('user_42', { plan: 'pro', seats: 3, nested: { no: true } as never });
  await i.track('created_project', { template: 'blank' });
  await i.flush();
  assert.deepEqual(c.received.map((r) => [r.type, r.payload.url, r.payload.name ?? null, r.payload.user ?? null]), [
    ['event', '/home', null, null],
    ['identify', '/', null, 'user_42'],
    ['event', '/', 'created_project', 'user_42'],
  ]);
  assert.deepEqual(c.received[1]!.payload.data, { plan: 'pro', seats: 3 });
  assert.equal(c.received[0]!.payload.hostname, 'com.acme.shop');
  assert.equal(c.received[0]!.payload.screen, '390x844');
  // A restarted app still knows the person until reset.
  const again = createInsights({ site: SITE, collector: 'https://collect.test', storage: s, fetch: c.fetch, setTimeout: t.setTimeout });
  await again.track('opened');
  await again.flush();
  assert.equal(c.received.at(-1)!.payload.user, 'user_42');
  assert.equal(again.userId(), 'user_42');
  await again.reset();
  await again.track('signed_out_view');
  await again.flush();
  assert.equal(c.received.at(-1)!.payload.user, undefined);
  const third = createInsights({ site: SITE, collector: 'https://collect.test', storage: s, fetch: c.fetch, setTimeout: t.setTimeout });
  await third.track('x');
  assert.equal(third.userId(), null);
});

test('offline for ten minutes: nothing is lost and every event is sent once when the network is back', async () => {
  const c = collector();
  const s = memoryStorage();
  const t = later();
  let now = 0;
  const i = createInsights({ site: SITE, collector: 'https://collect.test', storage: s, fetch: c.fetch, now: () => now, setTimeout: t.setTimeout, retryMs: 15_000 });
  c.setOnline(false);
  for (let k = 0; k < 40; k += 1) {
    now += 15_000;
    await i.track('tick', { k });
  }
  assert.equal(c.received.length, 0);
  assert.equal(i.pending(), 40);
  // The app is killed and restarted while still offline: the queue is on disk.
  const restarted = createInsights({ site: SITE, collector: 'https://collect.test', storage: s, fetch: c.fetch, setTimeout: t.setTimeout });
  c.setOnline(true);
  assert.equal(await restarted.flush(), 40);
  // A retry timer from the first instance fires too: the collector sees no duplicate.
  for (const fn of t.timers) fn();
  await new Promise((r) => setTimeout(r, 10));
  await i.flush();
  assert.equal(c.received.length, 40);
  assert.deepEqual(c.received.map((r) => (r.payload.data as { k: number }).k), Array.from({ length: 40 }, (_, k) => k));
});

test('consent refused: nothing is queued or sent; bad input is refused plainly', async () => {
  const c = collector();
  const i = createInsights({ site: SITE, collector: 'https://collect.test', fetch: c.fetch, consent: () => false });
  await i.track('x');
  await i.screen('/y');
  assert.equal(i.pending(), 0);
  assert.equal(c.received.length, 0);
  await assert.rejects(i.identify('has space'), /identify needs the app user id/);
  assert.throws(() => createInsights({ site: 'nope', collector: 'https://c.test', fetch: c.fetch }), /site key/);
  assert.throws(() => createInsights({ site: SITE, collector: 'ftp://c.test', fetch: c.fetch }), /collector URL/);
});
