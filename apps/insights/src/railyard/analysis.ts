/**
 * Retention by property and breakdowns (fork addition, PY3.2). Both are one
 * SQL statement scoped by the org through `website.team_id`; dimensions and
 * properties come from a fixed list, and trait or event-data keys are bound
 * parameters, never SQL text.
 *
 * A person is the user id from identify when there is one, otherwise the
 * day's anonymous session (visitor ids rotate daily by design, so anonymous
 * people never carry over to the next day).
 */

// Kept import-free so node:test runs these modules without the app's build.
const isUuid = (v: unknown): v is string =>
  typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v);
const KEY = /^[A-Za-z0-9_.-]{1,50}$/;
const EVENT = /^[A-Za-z0-9 _.:/-]{1,50}$/;

export interface Query {
  query<T>(sql: string, data: Record<string, unknown>): Promise<T[]>;
}

const SESSION_PROPS: Record<string, string> = { country: 's.country', language: 's.language', screen: 's.screen' };

/** The SQL for a property of a person at their first activity; null when the name is not allowed. */
function propertyExpr(property: string | null): { expr: string; key: string | null } | null {
  if (property === null) return { expr: `'all'`, key: null };
  if (SESSION_PROPS[property]) return { expr: SESSION_PROPS[property] as string, key: null };
  const m = /^trait\.(.+)$/.exec(property);
  if (m && KEY.test(m[1] as string)) return { expr: 'p.traits ->> {{traitKey}}', key: m[1] as string };
  return null;
}

export function retentionSql(propExpr: string): string {
  return `
  with act as (
    select e.website_id::text || ':' || coalesce(s.distinct_id, e.session_id::text) as person,
           coalesce(s.distinct_id, e.session_id::text) as person_id,
           e.website_id, e.session_id, e.created_at,
           date_trunc({{unit}}, e.created_at, 'UTC') as bucket
    from website_event e
    join website w on w.website_id = e.website_id
    join session s on s.session_id = e.session_id and s.website_id = e.website_id
    where w.team_id = {{orgId::uuid}} and w.deleted_at is null
      and ({{appId}}::uuid is null or e.website_id = {{appId}}::uuid)
      and e.created_at >= {{since::timestamptz}}
      and ({{event}}::text is null or (e.event_type = 2 and e.event_name = {{event}}::text))
  ),
  firsts as (
    select distinct on (person) person, person_id, website_id, session_id, bucket as first_bucket
    from act order by person, created_at
  ),
  props as (
    select f.person, f.first_bucket, ${propExpr} as value
    from firsts f
    left join session s on s.session_id = f.session_id and s.website_id = f.website_id
    left join railyard_profile p on p.website_id = f.website_id and p.distinct_id = f.person_id
  ),
  returns as (
    select distinct a.person, round(extract(epoch from (a.bucket - p.first_bucket)) / {{unitSeconds::int}})::int as n
    from act a join props p using (person)
  )
  select coalesce(p.value, '(none)') as value, r.n, count(distinct r.person) as people
  from props p join returns r using (person)
  where r.n between 0 and {{periods::int}}
  group by 1, 2
  order by 1, 2`;
}

export interface RetentionRow {
  value: string;
  /** People whose first activity in the window was in period 0. */
  people: number;
  /** For periods 1..N: how many of them were active again in that period. */
  returned: number[];
  /** returned / people, rounded to 4 places (0 when people is 0). */
  rate: number[];
}

export async function retention(
  db: Query,
  orgId: string,
  q: { app?: string | null; period?: string; periods?: number; days?: number; property?: string | null; event?: string | null },
  now: Date,
): Promise<{ period: 'day' | 'week'; periods: number; property: string | null; since: string; rows: RetentionRow[] } | string> {
  if (!isUuid(orgId)) return 'org must be a UUID';
  const appId = q.app ?? null;
  if (appId !== null && !isUuid(appId)) return 'app must be a UUID';
  const period = q.period ?? 'day';
  if (period !== 'day' && period !== 'week') return 'period must be day or week';
  const periods = q.periods ?? (period === 'day' ? 7 : 8);
  if (!Number.isInteger(periods) || periods < 1 || periods > 12) return 'periods must be 1 to 12';
  const days = q.days ?? (period === 'day' ? 30 : 90);
  if (!Number.isInteger(days) || days < 1 || days > 180) return 'days must be 1 to 180';
  const event = q.event ?? null;
  if (event !== null && !EVENT.test(event)) return 'event is not a valid event name';
  const prop = propertyExpr(q.property ?? null);
  if (!prop) return 'property must be country, language, screen or trait.<key>';
  const since = new Date(now.getTime() - days * 86_400_000).toISOString();
  const rows = await db.query<{ value: string; n: number | string; people: number | string }>(retentionSql(prop.expr), {
    unit: period,
    orgId,
    appId,
    since,
    event,
    traitKey: prop.key,
    unitSeconds: period === 'day' ? 86_400 : 604_800,
    periods,
  });
  const byValue = new Map<string, number[]>();
  for (const r of rows) {
    const arr = byValue.get(r.value) ?? new Array(periods + 1).fill(0);
    arr[Number(r.n)] = Number(r.people);
    byValue.set(r.value, arr);
  }
  const out: RetentionRow[] = [...byValue.entries()].map(([value, arr]) => {
    const people = arr[0] as number;
    const returned = arr.slice(1);
    return { value, people, returned, rate: returned.map((x) => (people ? Math.round((x / people) * 10_000) / 10_000 : 0)) };
  });
  out.sort((a, b) => b.people - a.people || a.value.localeCompare(b.value));
  return { period, periods, property: q.property ?? null, since, rows: out };
}

const EVENT_DIMS: Record<string, string> = {
  path: 'e.url_path',
  referrer: 'e.referrer_domain',
  country: 's.country',
  language: 's.language',
  screen: 's.screen',
  event: 'e.event_name',
  hostname: 'e.hostname',
};

function dimensionExpr(dimension: string): { expr: string; key: string | null; needsPerson: boolean } | null {
  if (EVENT_DIMS[dimension]) return { expr: EVENT_DIMS[dimension] as string, key: null, needsPerson: false };
  const data = /^data\.(.+)$/.exec(dimension);
  if (data && KEY.test(data[1] as string)) {
    return {
      expr: '(select coalesce(d.string_value, d.number_value::text) from event_data d where d.website_event_id = e.event_id and d.website_id = e.website_id and d.data_key = {{dimKey}} limit 1)',
      key: data[1] as string,
      needsPerson: false,
    };
  }
  const trait = /^trait\.(.+)$/.exec(dimension);
  if (trait && KEY.test(trait[1] as string)) return { expr: 'p.traits ->> {{dimKey}}', key: trait[1] as string, needsPerson: true };
  return null;
}

export function breakdownSql(expr: string, needsPerson: boolean): string {
  const person = needsPerson ? 'left join railyard_profile p on p.website_id = e.website_id and p.distinct_id = s.distinct_id' : '';
  return `
  select coalesce((${expr})::text, '(none)') as value, count(*) as events, count(distinct e.session_id) as visitors
  from website_event e
  join website w on w.website_id = e.website_id
  left join session s on s.session_id = e.session_id and s.website_id = e.website_id
  ${person}
  where w.team_id = {{orgId::uuid}} and w.deleted_at is null
    and ({{appId}}::uuid is null or e.website_id = {{appId}}::uuid)
    and e.created_at >= {{since::timestamptz}} and e.created_at < {{until::timestamptz}}
    and ({{event}}::text is null or (e.event_type = 2 and e.event_name = {{event}}::text))
  group by 1
  order by events desc, value
  limit {{limit::int}}`;
}

export async function breakdown(
  db: Query,
  orgId: string,
  q: { app?: string | null; dimension?: string; days?: number; event?: string | null; limit?: number },
  now: Date,
): Promise<{ dimension: string; since: string; rows: { value: string; events: number; visitors: number }[] } | string> {
  if (!isUuid(orgId)) return 'org must be a UUID';
  const appId = q.app ?? null;
  if (appId !== null && !isUuid(appId)) return 'app must be a UUID';
  const dim = dimensionExpr(q.dimension ?? '');
  if (!dim) return 'dimension must be path, referrer, country, language, screen, event, hostname, data.<key> or trait.<key>';
  const days = q.days ?? 7;
  if (!Number.isInteger(days) || days < 1 || days > 180) return 'days must be 1 to 180';
  const event = q.event ?? null;
  if (event !== null && !EVENT.test(event)) return 'event is not a valid event name';
  const limit = q.limit ?? 50;
  if (!Number.isInteger(limit) || limit < 1 || limit > 500) return 'limit must be 1 to 500';
  const since = new Date(now.getTime() - days * 86_400_000).toISOString();
  const rows = await db.query<{ value: string; events: number | string; visitors: number | string }>(breakdownSql(dim.expr, dim.needsPerson), {
    dimKey: dim.key,
    orgId,
    appId,
    since,
    until: now.toISOString(),
    event,
    limit,
  });
  return { dimension: q.dimension as string, since, rows: rows.map((r) => ({ value: r.value, events: Number(r.events), visitors: Number(r.visitors) })) };
}
