// Railyard fork additions, without the app build:
//   node --test src/railyard/*.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { bearerOk, ingest, parseIngest, toRows } from './ingest.ts';
import { allowedPath } from './api-only.ts';
import { toPositional, RECEIPT_SQL } from './tenancy.ts';

const HERE = dirname(fileURLToPath(import.meta.url));
const ORG_A = '0b8f6a3e-1111-4a3e-8b8f-0b8f6a3e1111';
const ORG_B = '0b8f6a3e-2222-4a3e-8b8f-0b8f6a3e2222';
const APP_A = '5d1c0000-aaaa-4aaa-8aaa-5d1c0000aaaa';
const id = (...p) => {
  const h = createHash('sha256').update(p.join('|')).digest('hex');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-5${h.slice(13, 16)}-8${h.slice(17, 20)}-${h.slice(20, 32)}`;
};
const ev = (over = {}) => ({
  type: 'event', name: null, url: '/pricing?ref=x', hostname: 'shop.example', referrer: 'https://news.example/a', title: 'Pricing',
  language: 'en-US', screen: '1440x900', data: null, visitor: '3f1e'.repeat(8), country: 'DE', ts: '2026-09-29T12:00:01.512Z', ...over,
});
const body = (over = {}) => ({ org_id: ORG_A, sent_at: '2026-09-29T12:00:02.000Z', batches: [{ site_key: 'ry_site_abcdefgh12', app_id: APP_A, events: [ev(), ev({ name: 'signup' })] }], ...over });

test('the collector body is validated; nothing IP-shaped is accepted or produced', () => {
  const ok = parseIngest(body());
  assert.notEqual(typeof ok, 'string');
  assert.equal(parseIngest({ ...body(), org_id: 'nope' }), 'org_id must be a UUID');
  assert.equal(parseIngest(body({ batches: [{ site_key: 'bad', app_id: APP_A, events: [] }] })), 'site_key is malformed');
  assert.equal(parseIngest(body({ batches: [{ site_key: 'ry_site_abcdefgh12', app_id: APP_A, events: [ev({ visitor: '1.2.3.4' })] }] })), 'an event is malformed');
  assert.equal(parseIngest(body({ batches: [{ site_key: 'ry_site_abcdefgh12', app_id: APP_A, events: [ev({ name: 'x'.repeat(51) })] }] })), 'an event is malformed');
  const rows = toRows(APP_A, ev({ ip: '1.2.3.4' }), id);
  assert.ok(!JSON.stringify(rows).includes('1.2.3.4'));
  assert.equal(rows.event.urlPath, '/pricing');
  assert.equal(rows.event.urlQuery, 'ref=x');
  assert.equal(rows.event.referrerDomain, 'news.example');
  assert.equal(rows.event.eventType, 1);
  assert.equal(toRows(APP_A, ev({ name: 'signup' }), id).event.eventType, 2);
  // The same visitor on the same day is one session; the next day is another.
  assert.equal(toRows(APP_A, ev(), id).session.id, toRows(APP_A, ev({ ts: '2026-09-29T23:59:00Z' }), id).session.id);
  assert.notEqual(toRows(APP_A, ev(), id).session.id, toRows(APP_A, ev({ ts: '2026-09-30T00:00:01Z' }), id).session.id);
});

function fakeStore() {
  const s = { receipts: new Set(), websites: new Map(), sessions: new Map(), events: [] };
  return {
    s,
    deps: {
      receipt: async (o, t, k) => {
        const key = `${o}|${t}|${k}`;
        if (s.receipts.has(key)) return false;
        s.receipts.add(key);
        return true;
      },
      ensureWebsite: async (org, app) => {
        if (!s.websites.has(app)) s.websites.set(app, org);
        return s.websites.get(app) === org;
      },
      createSession: async (r) => void s.sessions.set(r.id, r),
      saveEvent: async (r) => void s.events.push(r),
      id,
    },
  };
}

test('a retried batch is stored once; an app id owned by another org is refused', async () => {
  const f = fakeStore();
  const first = await ingest(f.deps, parseIngest(body()));
  assert.deepEqual(first, { status: 200, body: { stored: 2, duplicate_batches: 0 } });
  const retry = await ingest(f.deps, parseIngest(body()));
  assert.deepEqual(retry.body, { stored: 0, duplicate_batches: 1 });
  assert.equal(f.s.events.length, 2);
  assert.equal(f.s.sessions.size, 1);
  const stolen = await ingest(f.deps, parseIngest(body({ org_id: ORG_B, sent_at: '2026-09-29T12:00:04.000Z' })));
  assert.equal(stolen.status, 409);
  assert.equal(f.s.events.length, 2);
});

test('bearer tokens: exact match, rotation, nothing short', () => {
  const t = 'a'.repeat(32);
  assert.equal(bearerOk(`Bearer ${t}`, [t]), true);
  assert.equal(bearerOk(`Bearer ${t}`, [undefined, t]), true);
  assert.equal(bearerOk(`Bearer ${'b'.repeat(32)}`, [t]), false);
  assert.equal(bearerOk('Bearer short', ['short']), false);
  assert.equal(bearerOk(null, [t]), false);
});

test('API-only mode serves only the Railyard API and the heartbeat', () => {
  for (const p of ['/v1/ingest/batch', '/api/v1/tenants/x/summary', '/api/heartbeat']) assert.equal(allowedPath(p, true), true, p);
  for (const p of ['/', '/login', '/websites', '/script.js', '/api/send', '/share/x']) assert.equal(allowedPath(p, true), false, p);
  assert.equal(allowedPath('/login', false), true);
});

test('placeholders become bound parameters, never interpolated text', () => {
  const { text, values } = toPositional(RECEIPT_SQL, { orgId: ORG_A, sentAt: "x'); drop table website; --", siteKey: 'k' });
  assert.match(text, /values \(\$1::uuid, \$2, \$3\)/);
  assert.equal(values[1], "x'); drop table website; --");
  assert.ok(!text.includes('drop table'));
});

test('no fork addition or NOTICE-less file names the upstream product in text a customer can see', () => {
  const files = [];
  const walk = (d) => {
    for (const f of readdirSync(d)) {
      const p = join(d, f);
      if (statSync(p).isDirectory()) walk(p);
      else if (/\.(ts|tsx)$/.test(f)) files.push(p);
    }
  };
  walk(HERE);
  walk(join(HERE, '../app/api/v1'));
  for (const f of files) {
    const strings = [...readFileSync(f, 'utf8').matchAll(/(['"`])((?:(?!\1)[^\\\n])+)\1/g)].map((m) => m[2]);
    for (const s of strings) assert.doesNotMatch(s, /umami/i, `${f}: ${s}`);
  }
});
