/**
 * Web vitals per app (fork addition, PY3.3): the 75th percentile of LCP, INP,
 * CLS, FCP and TTFB from page views (`performance` events, upstream event type
 * 5), overall, per UTC day, and before and after given deploy times. Scoped by
 * the org through `website.team_id`; bound parameters only.
 *
 * p75 is the figure Chrome's own field reporting uses to rate a page; the
 * ratings use the published thresholds (good / needs improvement / poor).
 */

// Kept import-free so node:test runs these modules without the app's build.
const isUuid = (v: unknown): v is string =>
  typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v);

export const METRICS = ['lcp', 'inp', 'cls', 'fcp', 'ttfb'] as const;
export type Metric = (typeof METRICS)[number];

/** [good at or below, poor above]. */
export const THRESHOLDS: Record<Metric, [number, number]> = {
  lcp: [2500, 4000],
  inp: [200, 500],
  cls: [0.1, 0.25],
  fcp: [1800, 3000],
  ttfb: [800, 1800],
};

export type Rating = 'good' | 'needs-improvement' | 'poor';

export function rate(metric: Metric, value: number | null): Rating | null {
  if (value === null) return null;
  const [good, poor] = THRESHOLDS[metric];
  return value <= good ? 'good' : value > poor ? 'poor' : 'needs-improvement';
}

const P75 = METRICS.map((m) => `percentile_cont(0.75) within group (order by e.${m}) as ${m}_p75`).join(',\n    ');

const FROM = `
  from website_event e
  join website w on w.website_id = e.website_id
  where w.team_id = {{orgId::uuid}} and w.deleted_at is null
    and ({{appId}}::uuid is null or e.website_id = {{appId}}::uuid)
    and e.event_type = 5
    and e.created_at >= {{from::timestamptz}} and e.created_at < {{to::timestamptz}}`;

export const VITALS_SQL = `
  select e.website_id::text as app_id,
    ${P75},
    count(*) as samples
  ${FROM}
  group by e.website_id
  order by e.website_id`;

export const VITALS_DAILY_SQL = `
  select e.website_id::text as app_id, date_trunc('day', e.created_at, 'UTC') as day,
    ${P75},
    count(*) as samples
  ${FROM}
  group by 1, 2
  order by 2, 1`;

export interface Query {
  query<T>(sql: string, data: Record<string, unknown>): Promise<T[]>;
}

export interface MetricValue {
  p75: number | null;
  rating: Rating | null;
}

export type VitalsRow = { app_id: string; samples: number } & Record<Metric, MetricValue>;

const num = (v: unknown): number | null => (v === null || v === undefined || v === '' ? null : Math.round(Number(v) * 1000) / 1000);

function toRow(r: Record<string, unknown>): VitalsRow {
  const out = { app_id: String(r.app_id), samples: Number(r.samples) } as VitalsRow;
  for (const m of METRICS) {
    const p75 = num(r[`${m}_p75`]);
    out[m] = { p75, rating: rate(m, p75) };
  }
  return out;
}

export interface DeployComparison {
  at: string;
  before: VitalsRow | null;
  after: VitalsRow | null;
}

/**
 * `GET /api/v1/tenants/{org}/vitals?app=&days=&marks=<iso,iso>`: overall and
 * daily p75 over the window, and for each deploy time (at most 10) the p75 in
 * the 24 hours before and after it.
 */
export async function vitals(
  db: Query,
  orgId: string,
  q: { app?: string | null; days?: number; marks?: string | null },
  now: Date,
): Promise<{ since: string; apps: VitalsRow[]; daily: (VitalsRow & { day: string })[]; deploys: DeployComparison[] } | string> {
  if (!isUuid(orgId)) return 'org must be a UUID';
  const appId = q.app ?? null;
  if (appId !== null && !isUuid(appId)) return 'app must be a UUID';
  const days = q.days ?? 7;
  if (!Number.isInteger(days) || days < 1 || days > 90) return 'days must be 1 to 90';
  const marks = (q.marks ?? '').split(',').filter(Boolean);
  if (marks.length > 10 || marks.some((m) => Number.isNaN(Date.parse(m)))) return 'marks must be up to 10 times';
  if (marks.length && appId === null) return 'marks need app (deploys belong to one app)';
  const from = new Date(now.getTime() - days * 86_400_000).toISOString();
  const to = now.toISOString();
  const apps = (await db.query<Record<string, unknown>>(VITALS_SQL, { orgId, appId, from, to })).map(toRow);
  const daily = (await db.query<Record<string, unknown>>(VITALS_DAILY_SQL, { orgId, appId, from, to })).map((r) => ({ ...toRow(r), day: new Date(r.day as string).toISOString().slice(0, 10) }));
  const deploys: DeployComparison[] = [];
  for (const m of marks) {
    const at = new Date(m);
    const window = (a: number, b: number) =>
      db.query<Record<string, unknown>>(VITALS_SQL, { orgId, appId, from: new Date(at.getTime() + a).toISOString(), to: new Date(at.getTime() + b).toISOString() });
    const [before] = await window(-86_400_000, 0);
    const [after] = await window(0, 86_400_000);
    deploys.push({ at: at.toISOString(), before: before ? toRow(before) : null, after: after ? toRow(after) : null });
  }
  return { since: from, apps, daily, deploys };
}
