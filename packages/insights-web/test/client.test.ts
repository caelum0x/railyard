// @railyard/insights-web (Wave P PY3.3) in a stand-in browser: page views,
// route changes, identify, and one performance event per page view whose
// payload passes the edge collector's own parser.
//   node --test test/*.test.ts
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { createInsightsWeb, type PerfEntry, type WebEnv } from '../src/index.ts';
import { parseOne } from '../../../apps/insights-edge/src/payload.mjs';

function browser(opts: { dnt?: string; supported?: string[] } = {}) {
  const sent: { type: string; payload: Record<string, unknown> }[] = [];
  const listeners: Record<string, (() => void)[]> = {};
  const observers: { type: string; cb: (l: { getEntries(): PerfEntry[] }) => void }[] = [];
  const on = (t: string, f: () => void) => void (listeners[t] = [...(listeners[t] ?? []), f]);
  const env: WebEnv = {
    location: { pathname: '/', search: '', hostname: 'shop.example' },
    document: { referrer: '', title: 'Shop', visibilityState: 'visible', addEventListener: on },
    navigator: {
      doNotTrack: opts.dnt ?? '0',
      language: 'en-US',
      sendBeacon: (_url, data) => {
        const p = JSON.parse((data as unknown as { text: string }).text);
        const parsed = parseOne(p);
        assert.ok(!('error' in parsed), JSON.stringify(parsed));
        sent.push(p);
        return true;
      },
    },
    screen: { width: 1440, height: 900 },
    history: { pushState() {} },
    addEventListener: on,
    PerformanceObserver: Object.assign(
      class {
        cb: (l: { getEntries(): PerfEntry[] }) => void;
        constructor(cb: (l: { getEntries(): PerfEntry[] }) => void) {
          this.cb = cb;
        }
        observe(o: Record<string, unknown>) {
          observers.push({ type: String(o.type), cb: this.cb });
        }
      },
      { supportedEntryTypes: opts.supported ?? ['navigation', 'paint', 'largest-contentful-paint', 'layout-shift', 'event', 'first-input'] },
    ),
    performance: { interactionCount: 3 },
    Blob: class {
      text: string;
      constructor(parts: string[]) {
        this.text = parts.join('');
      }
    } as unknown as WebEnv['Blob'],
  };
  const emit = (entries: PerfEntry[]) => {
    for (const e of entries) for (const o of observers.filter((x) => x.type === e.entryType)) o.cb({ getEntries: () => [e] });
  };
  const fire = (t: string) => (listeners[t] ?? []).forEach((f) => f());
  return { env, sent, emit, fire };
}

test('a page view, then the page speed of that view once when the page is hidden', () => {
  const b = browser();
  createInsightsWeb({ site: 'ry_site_abcdefgh1234', collector: 'https://collect.test/', env: b.env });
  b.emit([
    { entryType: 'navigation', name: '', startTime: 0, duration: 900, responseStart: 180 },
    { entryType: 'paint', name: 'first-contentful-paint', startTime: 600, duration: 0 },
    { entryType: 'largest-contentful-paint', name: '', startTime: 1400, duration: 0 },
    { entryType: 'layout-shift', name: '', startTime: 800, duration: 0, value: 0.04 },
    { entryType: 'event', name: 'click', startTime: 3000, duration: 72, interactionId: 5 },
  ]);
  b.env.document.visibilityState = 'hidden';
  b.fire('visibilitychange');
  b.fire('pagehide');
  assert.deepEqual(b.sent.map((s) => s.type), ['event', 'performance']);
  const perf = b.sent[1]!.payload;
  assert.deepEqual([perf.ttfb, perf.fcp, perf.lcp, perf.cls, perf.inp], [180, 600, 1400, 0.04, 72]);
  assert.equal(perf.url, '/');
});

test('a client-side route change sends the previous view\'s metrics, then a new page view; identify tags later events', () => {
  const b = browser();
  const i = createInsightsWeb({ site: 'ry_site_abcdefgh1234', collector: 'https://collect.test', env: b.env });
  b.emit([{ entryType: 'layout-shift', name: '', startTime: 100, duration: 0, value: 0.2 }]);
  i.identify('user_42', { plan: 'pro' });
  b.env.location.pathname = '/pricing';
  b.env.history!.pushState({}, '', '/pricing');
  i.track('checkout_started', { plan: 'pro' });
  assert.deepEqual(b.sent.map((s) => [s.type, s.payload.url, s.payload.user ?? null]), [
    ['event', '/', null],
    ['identify', '/', 'user_42'],
    ['performance', '/', 'user_42'],
    ['event', '/pricing', 'user_42'],
    ['event', '/pricing', 'user_42'],
  ]);
  assert.equal(b.sent[2]!.payload.cls, 0.2);
  assert.throws(() => i.identify('bad id'), /identify needs/);
});

test('Do Not Track: nothing at all is sent; no layout-shift support: no CLS is made up', () => {
  const dnt = browser({ dnt: '1' });
  const i = createInsightsWeb({ site: 'ry_site_abcdefgh1234', collector: 'https://collect.test', env: dnt.env });
  i.track('x');
  dnt.fire('pagehide');
  assert.equal(dnt.sent.length, 0);
  const safari = browser({ supported: ['navigation', 'paint'] });
  createInsightsWeb({ site: 'ry_site_abcdefgh1234', collector: 'https://collect.test', env: safari.env });
  safari.emit([{ entryType: 'navigation', name: '', startTime: 0, duration: 1, responseStart: 90 }]);
  safari.fire('pagehide');
  const perf = safari.sent.find((s) => s.type === 'performance')!.payload;
  assert.equal(perf.ttfb, 90);
  assert.equal(perf.cls, undefined);
});
