/**
 * Saved cohorts (fork addition, PY3.2): a named set of identified people,
 * defined by what they did, did not do, a trait and when they were first seen.
 * The definition is validated here and turned into SQL from fixed fragments
 * with bound parameters only; members are counted live on each read.
 */

// Kept import-free so node:test runs these modules without the app's build.
const isUuid = (v: unknown): v is string =>
  typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v);
const EVENT = /^[A-Za-z0-9 _.:/-]{1,50}$/;
const TRAIT_KEY = /^[A-Za-z0-9_.-]{1,50}$/;

export interface CohortDefinition {
  app_id: string | null;
  /** Did this event at least `min_times` times in the last `within_days` days. */
  performed: { event: string; min_times: number; within_days: number } | null;
  /** Did not do this event in the last `within_days` days. */
  not_performed: { event: string; within_days: number } | null;
  /** A trait from identify equals this value. */
  trait: { key: string; equals: string | number | boolean } | null;
  /** First seen within the last N days. */
  first_seen_within_days: number | null;
}

const days = (v: unknown, max = 365): number | null => (Number.isInteger(v) && (v as number) >= 1 && (v as number) <= max ? (v as number) : null);

/** Validates a definition; a string says what is wrong. At least one condition is required. */
export function parseCohort(v: unknown): CohortDefinition | string {
  if (!v || typeof v !== 'object' || Array.isArray(v)) return 'definition must be an object';
  const d = v as Record<string, unknown>;
  const app = d.app_id ?? null;
  if (app !== null && !isUuid(app)) return 'app_id must be a UUID';
  let performed: CohortDefinition['performed'] = null;
  if (d.performed != null) {
    const p = d.performed as Record<string, unknown>;
    const within = days(p.within_days);
    const min = p.min_times === undefined ? 1 : p.min_times;
    if (typeof p.event !== 'string' || !EVENT.test(p.event) || within === null || !Number.isInteger(min) || (min as number) < 1 || (min as number) > 1000) {
      return 'performed needs event, within_days (1 to 365) and optional min_times (1 to 1000)';
    }
    performed = { event: p.event, min_times: min as number, within_days: within };
  }
  let notPerformed: CohortDefinition['not_performed'] = null;
  if (d.not_performed != null) {
    const p = d.not_performed as Record<string, unknown>;
    const within = days(p.within_days);
    if (typeof p.event !== 'string' || !EVENT.test(p.event) || within === null) return 'not_performed needs event and within_days (1 to 365)';
    notPerformed = { event: p.event, within_days: within };
  }
  let trait: CohortDefinition['trait'] = null;
  if (d.trait != null) {
    const t = d.trait as Record<string, unknown>;
    const ok = typeof t.equals === 'boolean' || (typeof t.equals === 'number' && Number.isFinite(t.equals)) || (typeof t.equals === 'string' && t.equals.length <= 500);
    if (typeof t.key !== 'string' || !TRAIT_KEY.test(t.key) || !ok) return 'trait needs key and equals (a string, number or boolean)';
    trait = { key: t.key, equals: t.equals as string | number | boolean };
  }
  const first = d.first_seen_within_days == null ? null : days(d.first_seen_within_days, 3650);
  if (d.first_seen_within_days != null && first === null) return 'first_seen_within_days must be 1 to 3650';
  if (!performed && !notPerformed && !trait && first === null) return 'add at least one condition';
  return { app_id: app as string | null, performed, not_performed: notPerformed, trait, first_seen_within_days: first };
}

/** A person's events: their linked sessions in the same app. */
const EVENTS_OF_PERSON = `from website_event e join session_link l on l.website_id = e.website_id and l.session_id = e.session_id
      where l.website_id = p.website_id and l.distinct_id = p.distinct_id and e.event_type = 2`;

/** The members query for a definition, and its parameters. */
export function cohortQuery(orgId: string, def: CohortDefinition, now: Date, opts: { count: boolean; limit?: number }): { sql: string; data: Record<string, unknown> } {
  const since = (n: number) => new Date(now.getTime() - n * 86_400_000).toISOString();
  const where = ['w.team_id = {{orgId::uuid}}', 'w.deleted_at is null'];
  const data: Record<string, unknown> = { orgId };
  if (def.app_id) {
    where.push('p.website_id = {{appId::uuid}}');
    data.appId = def.app_id;
  }
  if (def.first_seen_within_days !== null) {
    where.push('p.first_seen >= {{firstSince::timestamptz}}');
    data.firstSince = since(def.first_seen_within_days);
  }
  if (def.trait) {
    where.push('p.traits ->> {{traitKey}} = {{traitValue}}');
    data.traitKey = def.trait.key;
    data.traitValue = String(def.trait.equals);
  }
  if (def.performed) {
    where.push(`(select count(*) ${EVENTS_OF_PERSON} and e.event_name = {{doneEvent}} and e.created_at >= {{doneSince::timestamptz}}) >= {{doneTimes::int}}`);
    data.doneEvent = def.performed.event;
    data.doneSince = since(def.performed.within_days);
    data.doneTimes = def.performed.min_times;
  }
  if (def.not_performed) {
    where.push(`not exists (select 1 ${EVENTS_OF_PERSON} and e.event_name = {{notEvent}} and e.created_at >= {{notSince::timestamptz}})`);
    data.notEvent = def.not_performed.event;
    data.notSince = since(def.not_performed.within_days);
  }
  const from = `from railyard_profile p join website w on w.website_id = p.website_id where ${where.join(' and ')}`;
  if (opts.count) return { sql: `select count(*) as members ${from}`, data };
  data.limit = opts.limit ?? 1000;
  return { sql: `select p.website_id::text as app_id, p.distinct_id as user_id, p.last_seen ${from} order by p.last_seen desc, p.distinct_id limit {{limit::int}}`, data };
}

export const SAVE_COHORT_SQL = `
  insert into railyard_cohort (cohort_id, team_id, name, definition, created_at, updated_at)
  values ({{cohortId::uuid}}, {{orgId::uuid}}, {{name}}, {{definition::jsonb}}, {{at::timestamptz}}, {{at::timestamptz}})
  on conflict (team_id, name) do update set definition = excluded.definition, updated_at = excluded.updated_at
  returning cohort_id::text`;

export const LIST_COHORTS_SQL = `
  select cohort_id::text, name, definition, created_at, updated_at from railyard_cohort
  where team_id = {{orgId::uuid}} order by name`;

export const GET_COHORT_SQL = `
  select cohort_id::text, name, definition from railyard_cohort
  where team_id = {{orgId::uuid}} and cohort_id = {{cohortId::uuid}}`;

export const DELETE_COHORT_SQL = `
  delete from railyard_cohort where team_id = {{orgId::uuid}} and cohort_id = {{cohortId::uuid}} returning cohort_id`;

export interface Query {
  query<T>(sql: string, data: Record<string, unknown>): Promise<T[]>;
}

const NAME = /^[^\u0000-\u001f]{1,100}$/;
const defOf = (v: unknown) => (typeof v === 'string' ? JSON.parse(v) : v);

export async function saveCohort(db: Query, orgId: string, body: unknown, now: Date, newId: () => string) {
  if (!isUuid(orgId)) return 'org must be a UUID';
  const b = (body ?? {}) as Record<string, unknown>;
  if (typeof b.name !== 'string' || !NAME.test(b.name.trim())) return 'name must be 1 to 100 characters';
  const def = parseCohort(b.definition);
  if (typeof def === 'string') return def;
  const [row] = await db.query<{ cohort_id: string }>(SAVE_COHORT_SQL, { cohortId: newId(), orgId, name: b.name.trim(), definition: JSON.stringify(def), at: now.toISOString() });
  return { cohort_id: row?.cohort_id as string, name: b.name.trim(), definition: def };
}

export async function listCohorts(db: Query, orgId: string, now: Date) {
  if (!isUuid(orgId)) return 'org must be a UUID';
  const rows = await db.query<{ cohort_id: string; name: string; definition: unknown }>(LIST_COHORTS_SQL, { orgId });
  const out = [];
  for (const r of rows) {
    const def = parseCohort(defOf(r.definition));
    if (typeof def === 'string') continue;
    const q = cohortQuery(orgId, def, now, { count: true });
    const [c] = await db.query<{ members: number | string }>(q.sql, q.data);
    out.push({ cohort_id: r.cohort_id, name: r.name, definition: def, members: Number(c?.members ?? 0) });
  }
  return { cohorts: out };
}

export async function cohortMembers(db: Query, orgId: string, cohortId: string, now: Date, limit = 1000) {
  if (!isUuid(orgId) || !isUuid(cohortId)) return 'org and cohort must be UUIDs';
  const [r] = await db.query<{ cohort_id: string; name: string; definition: unknown }>(GET_COHORT_SQL, { orgId, cohortId });
  if (!r) return null;
  const def = parseCohort(defOf(r.definition));
  if (typeof def === 'string') return def;
  const c = cohortQuery(orgId, def, now, { count: true });
  const [count] = await db.query<{ members: number | string }>(c.sql, c.data);
  const m = cohortQuery(orgId, def, now, { count: false, limit });
  const members = await db.query<{ app_id: string; user_id: string; last_seen: unknown }>(m.sql, m.data);
  return {
    cohort_id: r.cohort_id,
    name: r.name,
    definition: def,
    members: Number(count?.members ?? 0),
    people: members.map((x) => ({ app_id: x.app_id, user_id: x.user_id, last_seen: new Date(x.last_seen as string).toISOString() })),
  };
}
