// Insights depth (PY3.2) at the SQL level: the upstream migrations plus the
// fork's 90 and 91 on a throwaway local cluster (never a hosted database).
// Identify and profiles, saved cohorts (checked against hand-written SQL),
// retention by property, breakdowns, CSV and JSON export, delete by user id,
// and tenant isolation for each. Skipped when pg_ctl/initdb are missing.
import { test, after } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { toPositional } from './tenancy.ts';
import { LINK_SESSION_SQL, UPSERT_PROFILE_SQL, eraseUser, listProfiles, profileEvents, sweepErasures } from './people.ts';
import { cohortMembers, cohortQuery, listCohorts, parseCohort, saveCohort } from './cohorts.ts';
import { breakdown, retention } from './analysis.ts';
import { exportRows, toCsv } from './export.ts';

const HERE = dirname(fileURLToPath(import.meta.url));
const MIGRATIONS = join(HERE, '../../prisma/migrations');
const have = spawnSync('pg_ctl', ['--version']).status === 0 && spawnSync('initdb', ['--version']).status === 0;
const ORG_A = '0b8f6a3e-1111-4a3e-8b8f-0b8f6a3e1111';
const ORG_B = '0b8f6a3e-2222-4a3e-8b8f-0b8f6a3e2222';
const APP_A = '5d1c0000-aaaa-4aaa-8aaa-5d1c0000aaaa';
const APP_B = '5d1c0000-bbbb-4bbb-8bbb-5d1c0000bbbb';
const NOW = new Date('2026-09-30T12:00:00Z');
const day = (n) => new Date(Date.UTC(2026, 8, 30 + n, 10, 0, 0)).toISOString();

/** A tiny CSV reader for psql --csv output (quotes and embedded newlines). */
function parseCsv(text) {
  const rows = [];
  let row = [];
  let cell = '';
  let q = false;
  for (let i = 0; i < text.length; i += 1) {
    const c = text[i];
    if (q) {
      if (c === '"' && text[i + 1] === '"') { cell += '"'; i += 1; } else if (c === '"') q = false; else cell += c;
    } else if (c === '"') q = true;
    else if (c === ',') { row.push(cell); cell = ''; }
    else if (c === '\n') { row.push(cell); rows.push(row); row = []; cell = ''; }
    else cell += c;
  }
  if (cell || row.length) { row.push(cell); rows.push(row); }
  return rows;
}

test('insights depth: people, cohorts, retention, breakdowns, export and delete by user, per org', { skip: have ? false : 'pg_ctl/initdb not installed' }, async () => {
  const dir = mkdtempSync(join(tmpdir(), 'ry-insights-depth-'));
  const port = String(52000 + Math.floor(Math.random() * 2000));
  after(() => {
    spawnSync('pg_ctl', ['-D', join(dir, 'data'), '-m', 'immediate', 'stop'], { stdio: 'ignore' });
    rmSync(dir, { recursive: true, force: true });
  });
  execFileSync('initdb', ['-D', join(dir, 'data'), '-U', 'postgres', '-A', 'trust', '--no-sync'], { stdio: 'ignore' });
  execFileSync('pg_ctl', ['-D', join(dir, 'data'), '-o', `-p ${port} -k ${dir} -c listen_addresses='' -c TimeZone=UTC`, '-w', 'start'], { stdio: 'ignore' });
  const psql = (args) => execFileSync('psql', ['-h', dir, '-p', port, '-U', 'postgres', '-v', 'ON_ERROR_STOP=1', '-q', ...args], { encoding: 'utf8' });
  for (const m of readdirSync(MIGRATIONS).filter((d) => /^\d+_/.test(d)).sort((a, b) => parseInt(a) - parseInt(b))) {
    psql(['-f', join(MIGRATIONS, m, 'migration.sql')]);
  }
  // The fork's migrations run again without error (idempotent).
  psql(['-f', join(MIGRATIONS, '90_railyard_ingest_receipts', 'migration.sql')]);
  psql(['-f', join(MIGRATIONS, '91_railyard_people', 'migration.sql')]);

  const lit = (v) => (v === null || v === undefined ? 'null' : `'${String(v).replace(/'/g, "''")}'`);
  let n = 0;
  const db = {
    async query(sql, data) {
      const { text, values } = toPositional(sql, data);
      const name = `q${++n}`;
      const out = psql(['--csv', '-c', `prepare ${name} as ${text.replace(/\s+/g, ' ')}; execute ${name}${values.length ? `(${values.map(lit).join(', ')})` : ''};`]);
      const rows = parseCsv(out);
      if (!rows.length) return [];
      const [head, ...body] = rows;
      return body.map((r) => Object.fromEntries(head.map((h, i) => [h, r[i] === '' ? null : r[i]])));
    },
  };
  const sql = (s) => psql(['-c', s]);

  // ---- fixtures: two orgs, the same user id "u1" in both ----
  for (const [org, app] of [[ORG_A, APP_A], [ORG_B, APP_B]]) {
    sql(`insert into team (team_id, name, created_at) values ('${org}', 'org', now())`);
    sql(`insert into website (website_id, name, team_id, created_at) values ('${app}', 'site', '${org}', now())`);
  }
  let sid = 0;
  const session = (app, at, country = 'DE') => {
    sid += 1;
    const id = `00000000-0000-4000-8000-${String(sid).padStart(12, '0')}`;
    sql(`insert into session (session_id, website_id, created_at, country, language, screen) values ('${id}', '${app}', '${at}', '${country}', 'en-US', '1440x900')`);
    return id;
  };
  let eid = 0;
  const event = (app, s, at, name, path = '/', data = null) => {
    eid += 1;
    const id = `00000000-0000-4000-9000-${String(eid).padStart(12, '0')}`;
    sql(`insert into website_event (event_id, website_id, session_id, visit_id, created_at, url_path, event_type, event_name) values ('${id}', '${app}', '${s}', gen_random_uuid(), '${at}', ${lit(path)}, ${name ? 2 : 1}, ${lit(name)})`);
    if (data) for (const [k, v] of Object.entries(data)) sql(`insert into event_data (event_data_id, website_id, website_event_id, data_key, string_value, data_type, created_at) values (gen_random_uuid(), '${app}', '${id}', '${k}', '${v}', 1, '${at}')`);
    return id;
  };
  const identify = async (app, s, user, traits, at) => {
    await db.query(LINK_SESSION_SQL, { websiteId: app, sessionId: s, distinctId: user, at });
    await db.query(UPSERT_PROFILE_SQL, { websiteId: app, distinctId: user, at, traits: JSON.stringify(traits) });
  };
  // Org A, u1 (pro): sign-up 10 days ago, projects 9, 5 and 1 days ago.
  const u1a = session(APP_A, day(-10));
  await identify(APP_A, u1a, 'u1', { plan: 'free' }, day(-10));
  event(APP_A, u1a, day(-10), 'signup', '/', { source: 'ads' });
  for (const d of [-9, -5, -1]) {
    const s = session(APP_A, day(d));
    await identify(APP_A, s, 'u1', d === -1 ? { plan: 'pro' } : {}, day(d));
    event(APP_A, s, day(d), 'created_project');
  }
  // u2 (free): one visit 3 days ago.
  const u2 = session(APP_A, day(-3), 'US');
  await identify(APP_A, u2, 'u2', { plan: 'free' }, day(-3));
  event(APP_A, u2, day(-3), 'signup', '/', { source: 'seo' });
  event(APP_A, u2, day(-3), 'created_project');
  // u3 (pro): active 40 and 35 days ago only.
  const u3 = session(APP_A, day(-40));
  await identify(APP_A, u3, 'u3', { plan: 'pro' }, day(-40));
  event(APP_A, u3, day(-40), 'signup');
  event(APP_A, session(APP_A, day(-35)), day(-35), 'created_project');
  // Two anonymous visits 2 days ago; one path would be a spreadsheet formula.
  event(APP_A, session(APP_A, day(-2), 'FR'), day(-2), null, '=HYPERLINK("http://x")');
  event(APP_A, session(APP_A, day(-2), 'FR'), day(-2), null, '/pricing');
  // Org B, also "u1", pro, five projects.
  const u1b = session(APP_B, day(-4));
  await identify(APP_B, u1b, 'u1', { plan: 'pro' }, day(-4));
  for (let i = 0; i < 5; i += 1) event(APP_B, u1b, day(-4), 'created_project');

  // ---- profiles ----
  const pa = await listProfiles(db, ORG_A, {});
  assert.deepEqual(pa.profiles.map((p) => [p.user_id, p.traits.plan, p.sessions]).sort(), [['u1', 'pro', 4], ['u2', 'free', 1], ['u3', 'pro', 1]]);
  const u1prof = pa.profiles.find((p) => p.user_id === 'u1');
  assert.equal(u1prof.first_seen, day(-10), 'first seen stays the earliest');
  assert.equal(u1prof.last_seen, day(-1));
  const pb = await listProfiles(db, ORG_B, {});
  assert.deepEqual(pb.profiles.map((p) => [p.app_id, p.user_id]), [[APP_B, 'u1']]);
  assert.deepEqual((await listProfiles(db, ORG_B, { app: APP_A })).profiles, [], 'org B naming org A\'s app sees nothing');
  assert.equal((await profileEvents(db, ORG_A, 'u1', null)).length, 4);
  assert.equal(typeof (await listProfiles(db, ORG_A, { user: 'bad id' })), 'string');

  // ---- cohorts: live counts equal hand-written SQL on the same fixtures ----
  const hand = async (text) => Number((await db.query(text, {}))[0].n);
  const pro2 = parseCohort({ performed: { event: 'created_project', min_times: 2, within_days: 30 }, trait: { key: 'plan', equals: 'pro' } });
  const q1 = cohortQuery(ORG_A, pro2, NOW, { count: true });
  const got1 = Number((await db.query(q1.sql, q1.data))[0].members);
  const want1 = await hand(`
    select count(*) as n from (
      select l.website_id, l.distinct_id from website_event e
      join session_link l on l.session_id = e.session_id and l.website_id = e.website_id
      join website w on w.website_id = e.website_id
      where w.team_id = '${ORG_A}' and e.event_name = 'created_project' and e.created_at >= timestamptz '${NOW.toISOString()}' - interval '30 days'
      group by l.website_id, l.distinct_id having count(*) >= 2) x
    join railyard_profile p on p.website_id = x.website_id and p.distinct_id = x.distinct_id
    where p.traits->>'plan' = 'pro'`);
  assert.equal(got1, want1);
  assert.equal(got1, 1);
  const lapsed = parseCohort({ not_performed: { event: 'created_project', within_days: 7 }, first_seen_within_days: 60 });
  const q2 = cohortQuery(ORG_A, lapsed, NOW, { count: true });
  const want2 = await hand(`
    select count(*) as n from railyard_profile p join website w on w.website_id = p.website_id
    where w.team_id = '${ORG_A}' and p.first_seen >= timestamptz '${NOW.toISOString()}' - interval '60 days'
      and p.distinct_id not in (
        select l.distinct_id from website_event e join session_link l on l.session_id = e.session_id and l.website_id = e.website_id
        where l.website_id = p.website_id and e.event_name = 'created_project' and e.created_at >= timestamptz '${NOW.toISOString()}' - interval '7 days')`);
  assert.equal(Number((await db.query(q2.sql, q2.data))[0].members), want2);
  assert.equal(want2, 1);
  let cid = 0;
  const newId = () => `c0000000-0000-4000-8000-${String(++cid).padStart(12, '0')}`;
  const saved = await saveCohort(db, ORG_A, { name: 'Active pro', definition: { performed: { event: 'created_project', min_times: 2, within_days: 30 }, trait: { key: 'plan', equals: 'pro' } } }, NOW, newId);
  const members = await cohortMembers(db, ORG_A, saved.cohort_id, NOW);
  assert.equal(members.members, 1);
  assert.deepEqual(members.people.map((p) => p.user_id), ['u1']);
  assert.deepEqual((await listCohorts(db, ORG_A, NOW)).cohorts.map((c) => [c.name, c.members]), [['Active pro', 1]]);
  assert.equal(await cohortMembers(db, ORG_B, saved.cohort_id, NOW), null, 'org B cannot read org A\'s cohort');
  // The same definition saved by org B counts only org B's people.
  const savedB = await saveCohort(db, ORG_B, { name: 'Active pro', definition: { performed: { event: 'created_project', min_times: 2, within_days: 30 } } }, NOW, newId);
  assert.equal((await cohortMembers(db, ORG_B, savedB.cohort_id, NOW)).members, 1);
  assert.equal(typeof (await saveCohort(db, ORG_A, { name: 'x', definition: {} }, NOW, newId)), 'string');

  // ---- retention by property (days, 7 periods, 30-day window) ----
  const r = await retention(db, ORG_A, { period: 'day', periods: 7, days: 30, property: 'trait.plan' }, NOW);
  assert.deepEqual(r.rows, [
    { value: '(none)', people: 2, returned: [0, 0, 0, 0, 0, 0, 0], rate: [0, 0, 0, 0, 0, 0, 0] },
    { value: 'free', people: 1, returned: [0, 0, 0, 0, 0, 0, 0], rate: [0, 0, 0, 0, 0, 0, 0] },
    { value: 'pro', people: 1, returned: [1, 0, 0, 0, 1, 0, 0], rate: [1, 0, 0, 0, 1, 0, 0] },
  ]);
  const byCountry = await retention(db, ORG_A, { period: 'week', periods: 2, days: 30, property: 'country' }, NOW);
  assert.deepEqual(byCountry.rows.map((x) => [x.value, x.people]), [['DE', 1], ['FR', 2], ['US', 1]].sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0])));
  assert.equal(typeof (await retention(db, ORG_A, { property: 'password' }, NOW)), 'string');
  const rb = await retention(db, ORG_B, { period: 'day', periods: 3, days: 30, property: null }, NOW);
  assert.deepEqual(rb.rows, [{ value: 'all', people: 1, returned: [0, 0, 0], rate: [0, 0, 0] }], 'org B sees only its own person');

  // ---- breakdowns ----
  const byEvent = await breakdown(db, ORG_A, { dimension: 'event', days: 30 }, NOW);
  assert.deepEqual(byEvent.rows.map((x) => [x.value, x.events]), [['created_project', 4], ['(none)', 2], ['signup', 2]]);
  const byPlan = await breakdown(db, ORG_A, { dimension: 'trait.plan', days: 30 }, NOW);
  assert.deepEqual(byPlan.rows.map((x) => [x.value, x.events, x.visitors]), [['pro', 4, 4], ['(none)', 2, 2], ['free', 2, 1]]);
  const bySource = await breakdown(db, ORG_A, { dimension: 'data.source', days: 30, event: 'signup' }, NOW);
  assert.deepEqual(bySource.rows.map((x) => [x.value, x.events]), [['ads', 1], ['seo', 1]]);
  const bByEvent = await breakdown(db, ORG_B, { dimension: 'event', days: 30, app: APP_A }, NOW);
  assert.deepEqual(bByEvent.rows, [], 'org B naming org A\'s app sees nothing');
  assert.equal(typeof (await breakdown(db, ORG_A, { dimension: "e.url_path; drop table session" }, NOW)), 'string');

  // ---- export ----
  const ev = await exportRows(db, ORG_A, { kind: 'events', from: new Date(NOW.getTime() - 31 * 86_400_000).toISOString(), to: NOW.toISOString() }, NOW);
  assert.equal(ev.rows.length, 8);
  assert.equal(ev.truncated, false);
  assert.ok(ev.rows.every((x) => x.app_id === APP_A));
  assert.equal(ev.rows.filter((x) => x.user_id === 'u1').length, 4);
  const csv = toCsv(ev.columns, ev.rows);
  const lines = csv.trim().split('\r\n');
  assert.equal(lines[0], 'time,app_id,type,name,path,query,referrer,title,hostname,country,language,screen,session_id,user_id');
  assert.equal(lines.length, 9);
  assert.ok(csv.includes(`"'=HYPERLINK(""http://x"")"`), 'a formula-looking cell is defused');
  const profiles = await exportRows(db, ORG_A, { kind: 'profiles' }, NOW);
  assert.deepEqual(profiles.rows.map((x) => x.user_id).sort(), ['u1', 'u2', 'u3']);
  assert.equal(typeof (await exportRows(db, ORG_A, { from: '2026-01-01T00:00:00Z', to: NOW.toISOString() }, NOW)), 'string', 'at most 31 days');
  assert.equal((await exportRows(db, ORG_B, { kind: 'events', app: APP_A }, NOW)).rows.length, 0);

  // ---- delete by user id ----
  let rid = 0;
  const reqId = () => `e0000000-0000-4000-8000-${String(++rid).padStart(12, '0')}`;
  const countA = () => Number(psql(['-Atc', `select count(*) from website_event where website_id = '${APP_A}'`]).trim());
  const before = countA();
  // Org B erasing "u1" removes only org B's u1.
  const eb = await eraseUser(db, ORG_B, 'u1', null, NOW, reqId);
  assert.equal(eb.status, 'done');
  assert.equal(countA(), before);
  assert.equal((await listProfiles(db, ORG_A, { user: 'u1' })).profiles.length, 1);
  assert.equal(Number(psql(['-Atc', `select count(*) from website_event where website_id = '${APP_B}'`]).trim()), 0);
  // Org A erases u1: events, event data, links, sessions and the profile go; u2 stays.
  const ea = await eraseUser(db, ORG_A, 'u1', null, NOW, reqId);
  assert.equal(ea.status, 'done');
  assert.ok(ea.rows_deleted >= 4 + 1 + 4 + 4 + 1, `rows deleted ${ea.rows_deleted}`);
  assert.equal(ea.due_by, new Date(NOW.getTime() + 86_400_000).toISOString());
  assert.equal(countA(), before - 4);
  const left = (t, where) => Number(psql(['-Atc', `select count(*) from ${t} where ${where}`]).trim());
  assert.equal(left('session_link', `distinct_id = 'u1'`), 0);
  assert.equal(left('session', `distinct_id = 'u1'`), 0);
  assert.equal(left('railyard_profile', `distinct_id = 'u1'`), 0);
  assert.equal(left('event_data', `string_value = 'ads'`), 0);
  assert.equal(left('event_data', `string_value = 'seo'`), 1);
  assert.deepEqual((await listProfiles(db, ORG_A, {})).profiles.map((p) => p.user_id).sort(), ['u2', 'u3']);
  assert.equal(left('railyard_erasure', 'done_at is not null'), 2);
  // A pending request (a failed first try) is finished by the sweep.
  sql(`insert into railyard_erasure (request_id, team_id, distinct_id, requested_at) values ('${reqId()}', '${ORG_A}', 'u2', now())`);
  assert.deepEqual(await sweepErasures(db), { done: 1, failed: 0 });
  assert.equal(left('railyard_profile', `distinct_id = 'u2'`), 0);
  assert.equal(left('railyard_erasure', 'done_at is null'), 0);
});
