// Tenant isolation at the SQL level: the upstream migrations plus the fork's
// receipt table on a throwaway local cluster (never a hosted database), then
// the summary queries for two orgs. Skipped when pg_ctl/initdb are missing.
import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { APPS_SQL, FUNNEL_SQL, summarize } from './summary.ts';
import { ENSURE_TEAM_SQL, ENSURE_WEBSITE_SQL, RECEIPT_SQL, WEBSITE_OF_ORG_SQL, toPositional } from './tenancy.ts';

const HERE = dirname(fileURLToPath(import.meta.url));
const MIGRATIONS = join(HERE, '../../prisma/migrations');
const have = spawnSync('pg_ctl', ['--version']).status === 0 && spawnSync('initdb', ['--version']).status === 0;
const ORG_A = '0b8f6a3e-1111-4a3e-8b8f-0b8f6a3e1111';
const ORG_B = '0b8f6a3e-2222-4a3e-8b8f-0b8f6a3e2222';
const APP_A = '5d1c0000-aaaa-4aaa-8aaa-5d1c0000aaaa';
const APP_B = '5d1c0000-bbbb-4bbb-8bbb-5d1c0000bbbb';

const lit = (v) => (v === null || v === undefined ? 'null' : `'${String(v).replace(/'/g, "''")}'`);

test('summaries only ever read the asking org; retried receipts are refused by the database', { skip: have ? false : 'pg_ctl/initdb not installed' }, async () => {
  const dir = mkdtempSync(join(tmpdir(), 'ry-insights-pg-'));
  const port = String(56000 + Math.floor(Math.random() * 2000));
  after(() => {
    spawnSync('pg_ctl', ['-D', join(dir, 'data'), '-m', 'immediate', 'stop'], { stdio: 'ignore' });
    rmSync(dir, { recursive: true, force: true });
  });
  execFileSync('initdb', ['-D', join(dir, 'data'), '-U', 'postgres', '-A', 'trust', '--no-sync'], { stdio: 'ignore' });
  execFileSync('pg_ctl', ['-D', join(dir, 'data'), '-o', `-p ${port} -k ${dir} -c listen_addresses=''`, '-w', 'start'], { stdio: 'ignore' });
  const psql = (args) => execFileSync('psql', ['-h', dir, '-p', port, '-U', 'postgres', '-v', 'ON_ERROR_STOP=1', '-qAt', ...args], { encoding: 'utf8' });
  for (const m of readdirSync(MIGRATIONS).filter((d) => /^\d+_/.test(d)).sort((a, b) => parseInt(a) - parseInt(b))) {
    psql(['-f', join(MIGRATIONS, m, 'migration.sql')]);
  }
  psql(['-f', join(MIGRATIONS, '90_railyard_ingest_receipts', 'migration.sql')]);
  let n = 0;
  const run = (sql, data) => {
    const { text, values } = toPositional(sql, data);
    const name = `q${++n}`;
    const out = psql(['-F', '\t', '-c', `prepare ${name} as ${text.replace(/\s+/g, ' ')}; execute ${name}${values.length ? `(${values.map(lit).join(', ')})` : ''};`]);
    return out.trim() ? out.trim().split('\n').map((l) => l.split('\t')) : [];
  };
  for (const [org, app] of [[ORG_A, APP_A], [ORG_B, APP_B]]) {
    run(ENSURE_TEAM_SQL, { orgId: org, name: 'org' });
    run(ENSURE_WEBSITE_SQL, { appId: app, orgId: org, name: 'ry_site_x', domain: 'shop.example' });
  }
  // A website id already owned by org A never becomes org B's.
  run(ENSURE_WEBSITE_SQL, { appId: APP_A, orgId: ORG_B, name: 'steal', domain: null });
  assert.equal(run(WEBSITE_OF_ORG_SQL, { appId: APP_A, orgId: ORG_B }).length, 0);
  assert.equal(run(WEBSITE_OF_ORG_SQL, { appId: APP_A, orgId: ORG_A }).length, 1);
  assert.equal(run(RECEIPT_SQL, { orgId: ORG_A, sentAt: 't1', siteKey: 'ry_site_x' }).length, 1);
  assert.equal(run(RECEIPT_SQL, { orgId: ORG_A, sentAt: 't1', siteKey: 'ry_site_x' }).length, 0);
  // Org A: 2 visits, 1 sign-up, 1 first action. Org B: 5 visits.
  const now = new Date('2026-09-29T12:00:00Z');
  const at = (m) => new Date(now.getTime() - m * 60_000).toISOString();
  const session = (sid, app) => psql(['-c', `insert into session (session_id, website_id, created_at) values ('${sid}', '${app}', now())`]);
  const event = (sid, app, type, name, minutesAgo) =>
    psql(['-c', `insert into website_event (event_id, website_id, session_id, visit_id, created_at, url_path, event_type, event_name) values (gen_random_uuid(), '${app}', '${sid}', gen_random_uuid(), '${at(minutesAgo)}', '/', ${type}, ${name ? `'${name}'` : 'null'})`]);
  const s = (i) => `00000000-0000-4000-8000-${String(i).padStart(12, '0')}`;
  session(s(1), APP_A); event(s(1), APP_A, 1, null, 30); event(s(1), APP_A, 2, 'signup', 29); event(s(1), APP_A, 2, 'created_project', 28);
  session(s(2), APP_A); event(s(2), APP_A, 1, null, 20);
  for (let i = 10; i < 15; i += 1) { session(s(i), APP_B); event(s(i), APP_B, 1, null, 10); }
  const query = async (sql, data) => {
    const rows = run(sql, data);
    if (sql === APPS_SQL) return rows.map(([app_id, first_event_at, events, visitors, month_events]) => ({ app_id, first_event_at: first_event_at || null, events, visitors, month_events }));
    return rows.map(([visits, signups, first_actions]) => ({ visits, signups, first_actions }));
  };
  const a = await summarize({ query }, ORG_A, null, 7, now);
  assert.deepEqual(a.apps.map((x) => [x.app_id, x.events, x.visitors, x.month_events]), [[APP_A, 4, 2, 4]]);
  assert.ok(a.apps[0].first_event_at);
  assert.deepEqual(a.funnel.steps.map((x) => x.sessions), [2, 1, 1]);
  const b = await summarize({ query }, ORG_B, null, 7, now);
  assert.deepEqual(b.apps.map((x) => [x.app_id, x.events]), [[APP_B, 5]]);
  assert.deepEqual(b.funnel.steps.map((x) => x.sessions), [5, 0, 0]);
  // Org B asking for org A's app sees nothing of it.
  const cross = await summarize({ query }, ORG_B, APP_A, 7, now);
  assert.deepEqual(cross.apps, []);
  assert.deepEqual(cross.funnel.steps.map((x) => x.sessions), [0, 0, 0]);
  assert.equal(typeof (await summarize({ query }, 'nope', null, 7, now)), 'string');
});
