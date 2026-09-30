// Web vitals (PY3.3) at the SQL level on a throwaway cluster: performance
// events from the collector land as upstream type 5 rows with their metrics;
// p75 per app, per day and around a deploy equal hand-computed values; org
// isolation. Skipped when pg_ctl/initdb are missing.
import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createHash } from 'node:crypto';

import { toPositional } from './tenancy.ts';
import { parseIngest, toRows } from './ingest.ts';
import { rate, vitals } from './vitals.ts';

const HERE = dirname(fileURLToPath(import.meta.url));
const MIGRATIONS = join(HERE, '../../prisma/migrations');
const have = spawnSync('pg_ctl', ['--version']).status === 0 && spawnSync('initdb', ['--version']).status === 0;
const ORG_A = '0b8f6a3e-1111-4a3e-8b8f-0b8f6a3e1111';
const ORG_B = '0b8f6a3e-2222-4a3e-8b8f-0b8f6a3e2222';
const APP_A = '5d1c0000-aaaa-4aaa-8aaa-5d1c0000aaaa';
const APP_B = '5d1c0000-bbbb-4bbb-8bbb-5d1c0000bbbb';
const NOW = new Date('2026-09-30T12:00:00Z');
const DEPLOY = '2026-09-29T12:00:00.000Z';
const id = (...p) => {
  const h = createHash('sha256').update(p.join('|')).digest('hex');
  return `${h.slice(0, 8)}-${h.slice(8, 12)}-5${h.slice(13, 16)}-8${h.slice(17, 20)}-${h.slice(20, 32)}`;
};

test('ratings follow the published thresholds', () => {
  assert.equal(rate('lcp', 2500), 'good');
  assert.equal(rate('lcp', 2501), 'needs-improvement');
  assert.equal(rate('lcp', 4001), 'poor');
  assert.equal(rate('cls', 0.1), 'good');
  assert.equal(rate('inp', 600), 'poor');
  assert.equal(rate('ttfb', null), null);
});

test('a performance event from the collector is validated and becomes an upstream type 5 row', () => {
  const body = (ev) => ({ org_id: ORG_A, sent_at: NOW.toISOString(), batches: [{ site_key: 'ry_site_abcdefgh12', app_id: APP_A, events: [ev] }] });
  const base = { type: 'performance', name: null, url: '/', hostname: 'shop.example', referrer: null, title: null, language: null, screen: null, data: null, visitor: 'a'.repeat(32), country: null, ts: NOW.toISOString() };
  const ok = parseIngest(body({ ...base, lcp: 2100, inp: 180, cls: 0.05, fcp: 900, ttfb: 300 }));
  assert.notEqual(typeof ok, 'string');
  const rows = toRows(APP_A, ok.batches[0].events[0], id);
  assert.equal(rows.event.eventType, 5);
  assert.deepEqual([rows.event.lcp, rows.event.inp, rows.event.cls, rows.event.fcp, rows.event.ttfb], [2100, 180, 0.05, 900, 300]);
  assert.equal(parseIngest(body({ ...base })), 'an event is malformed', 'a performance event needs a metric');
  assert.equal(parseIngest(body({ ...base, lcp: -1 })), 'an event is malformed');
  assert.equal(parseIngest(body({ ...base, cls: 101 })), 'an event is malformed');
  assert.equal(parseIngest(body({ ...base, type: 'event', name: 'x', lcp: 5 })).batches[0].events[0].vitals, null, 'metrics ride only on performance events');
});

test('p75 per app, per day and around a deploy equal hand-computed values; org B sees none of org A', { skip: have ? false : 'pg_ctl/initdb not installed' }, async () => {
  const dir = mkdtempSync(join(tmpdir(), 'ry-insights-vitals-'));
  const port = String(50000 + Math.floor(Math.random() * 1900));
  after(() => {
    spawnSync('pg_ctl', ['-D', join(dir, 'data'), '-m', 'immediate', 'stop'], { stdio: 'ignore' });
    rmSync(dir, { recursive: true, force: true });
  });
  execFileSync('initdb', ['-D', join(dir, 'data'), '-U', 'postgres', '-A', 'trust', '--no-sync'], { stdio: 'ignore' });
  execFileSync('pg_ctl', ['-D', join(dir, 'data'), '-o', `-p ${port} -k ${dir} -c listen_addresses='' -c TimeZone=UTC`, '-w', 'start'], { stdio: 'ignore' });
  const psql = (args) => execFileSync('psql', ['-h', dir, '-p', port, '-U', 'postgres', '-v', 'ON_ERROR_STOP=1', '-q', ...args], { encoding: 'utf8' });
  for (const m of readdirSync(MIGRATIONS).filter((d) => /^\d+_/.test(d)).sort((a, b) => parseInt(a) - parseInt(b))) psql(['-f', join(MIGRATIONS, m, 'migration.sql')]);
  const lit = (v) => (v === null || v === undefined ? 'null' : `'${String(v).replace(/'/g, "''")}'`);
  let n = 0;
  const db = {
    async query(sql, data) {
      const { text, values } = toPositional(sql, data);
      const name = `q${++n}`;
      const out = psql(['--csv', '-c', `prepare ${name} as ${text.replace(/\s+/g, ' ')}; execute ${name}${values.length ? `(${values.map(lit).join(', ')})` : ''};`]).trim();
      if (!out) return [];
      const [head, ...rows] = out.split('\n').map((l) => l.split(','));
      return rows.map((r) => Object.fromEntries(head.map((h, i) => [h, r[i] === '' ? null : r[i]])));
    },
  };
  for (const [org, app] of [[ORG_A, APP_A], [ORG_B, APP_B]]) {
    psql(['-c', `insert into team (team_id, name, created_at) values ('${org}', 'o', now()); insert into website (website_id, name, team_id, created_at) values ('${app}', 's', '${org}', now())`]);
  }
  const perf = (app, at, v) =>
    psql(['-c', `insert into website_event (event_id, website_id, session_id, visit_id, created_at, url_path, event_type, lcp, inp, cls, fcp, ttfb) values (gen_random_uuid(), '${app}', gen_random_uuid(), gen_random_uuid(), '${at}', '/', 5, ${v.lcp}, ${v.inp}, ${v.cls}, ${v.fcp}, ${v.ttfb})`]);
  // Before the deploy (the previous 24 h): fast. After: LCP got slower.
  const before = [1000, 1200, 1400, 1600];
  const afterLcp = [2000, 3000, 4000, 5000];
  before.forEach((l, i) => perf(APP_A, `2026-09-29T0${i}:00:00Z`, { lcp: l, inp: 100, cls: 0.01, fcp: 800, ttfb: 200 }));
  afterLcp.forEach((l, i) => perf(APP_A, `2026-09-29T1${3 + i}:00:00Z`, { lcp: l, inp: 150, cls: 0.02, fcp: 900, ttfb: 250 }));
  // A page view (type 1) is not a vitals sample; org B has its own slow page.
  psql(['-c', `insert into website_event (event_id, website_id, session_id, visit_id, created_at, url_path, event_type) values (gen_random_uuid(), '${APP_A}', gen_random_uuid(), gen_random_uuid(), '2026-09-29T15:00:00Z', '/', 1)`]);
  perf(APP_B, '2026-09-29T15:00:00Z', { lcp: 9000, inp: 900, cls: 0.9, fcp: 5000, ttfb: 3000 });

  const p75 = (xs) => {
    const s = [...xs].sort((a, b) => a - b);
    const pos = 0.75 * (s.length - 1);
    const lo = Math.floor(pos);
    return s[lo] + (s[Math.min(lo + 1, s.length - 1)] - s[lo]) * (pos - lo);
  };
  const r = await vitals(db, ORG_A, { app: APP_A, days: 7, marks: DEPLOY }, NOW);
  assert.equal(r.apps.length, 1);
  assert.equal(r.apps[0].samples, 8);
  assert.equal(r.apps[0].lcp.p75, p75([...before, ...afterLcp]));
  assert.equal(r.apps[0].lcp.rating, rate('lcp', p75([...before, ...afterLcp])));
  assert.deepEqual(r.daily.map((d) => [d.day, d.samples]), [['2026-09-29', 8]]);
  const [d] = r.deploys;
  assert.equal(d.at, DEPLOY);
  assert.equal(d.before.lcp.p75, p75(before));
  assert.equal(d.after.lcp.p75, p75(afterLcp));
  assert.equal(d.before.lcp.rating, 'good');
  assert.equal(d.after.lcp.rating, 'poor');
  assert.equal(d.after.cls.p75, 0.02);
  const b = await vitals(db, ORG_B, { app: APP_A, days: 7 }, NOW);
  assert.deepEqual(b.apps, [], 'org B naming org A\'s app sees nothing');
  const bAll = await vitals(db, ORG_B, {}, NOW);
  assert.deepEqual(bAll.apps.map((x) => [x.app_id, x.lcp.p75]), [[APP_B, 9000]]);
  assert.equal(typeof (await vitals(db, ORG_A, { marks: DEPLOY }, NOW)), 'string', 'marks need an app');
});
