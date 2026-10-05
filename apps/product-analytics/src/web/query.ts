// Web analytics over the product analytics store (QY1.2, A18): visitors,
// page views, sessions and bounce rate, a daily series, top pages, entry
// referrers and entry UTM values, and page speed at the 75th percentile.
//
// The shapes follow the upstream web analytics queries (MIT,
// products/web_analytics/backend/hogql_queries/stats_table.py and the web
// vitals tiles, commit 7b13402a6350; nothing from `ee/`): a page counts
// `$pageview` events; visitors are distinct people; referrer and UTM are the
// values of each session's first page view ("$direct" without a referrer);
// page speed reads `$web_vitals` events (`$web_vitals_<METRIC>_value`) and
// rates the p75 with the published good / needs-improvement thresholds.
// Every statement binds the project id and runs under row level security.

import type { Db } from "../db.ts";
import { ApiError } from "../replay/shared/console-guard.ts";

const DAY = 86_400_000;
export const MAX_RANGE_DAYS = 90;
export const DEFAULT_LIMIT = 10;
export const MAX_LIMIT = 50;

/** Thresholds of the published metric definitions: [good up to, poor from]. */
export const VITAL_THRESHOLDS = {
  LCP: [2500, 4000],
  INP: [200, 500],
  CLS: [0.1, 0.25],
  FCP: [1800, 3000],
  TTFB: [800, 1800],
} as const;
export type VitalName = keyof typeof VITAL_THRESHOLDS;

export interface WebQuery {
  from: Date;
  to: Date;
  limit: number;
}

export interface Row {
  value: string;
  visitors: number;
  views: number;
}

export interface Vital {
  p75: number | null;
  samples: number;
  rating: "good" | "needs_improvement" | "poor" | null;
}

export interface WebOverview {
  range: { from: string; to: string };
  totals: { visitors: number; views: number; sessions: number; bounce_rate: number | null };
  series: { day: string; visitors: number; views: number }[];
  pages: Row[];
  referrers: Row[];
  utm: { source: Row[]; medium: Row[]; campaign: Row[] };
  vitals: Record<VitalName, Vital>;
}

export function parseWebQuery(q: URLSearchParams, now: Date): WebQuery {
  const to = q.get("to") ? new Date(q.get("to")!) : now;
  const days = q.get("days");
  const from = q.get("from") ? new Date(q.get("from")!) : new Date(to.getTime() - (days ? Number(days) : 7) * DAY);
  if (Number.isNaN(from.getTime()) || Number.isNaN(to.getTime()) || from >= to) throw new ApiError(400, "from and to must be dates, from before to.");
  if (to.getTime() - from.getTime() > MAX_RANGE_DAYS * DAY) throw new ApiError(400, `The web view covers at most ${MAX_RANGE_DAYS} days.`);
  const limit = q.get("limit") === null ? DEFAULT_LIMIT : Number(q.get("limit"));
  if (!Number.isInteger(limit) || limit < 1 || limit > MAX_LIMIT) throw new ApiError(400, `limit must be a whole number from 1 to ${MAX_LIMIT}.`);
  return { from, to, limit };
}

// Page views of the range: person, session (the SDK's, else the id's UTC day),
// path (from $pathname, else from $current_url), entry fields.
const PAGEVIEWS = `
  select e.person_id,
         coalesce(e.session_id, e.distinct_id || ':' || to_char(e."timestamp" at time zone 'UTC', 'YYYY-MM-DD')) as sid,
         e."timestamp" as ts,
         coalesce(nullif(e.properties ->> '$pathname', ''),
                  nullif(substring(e.properties ->> '$current_url' from '^[A-Za-z][A-Za-z0-9+.-]*://[^/?#]*([^?#]*)'), ''),
                  '/') as path,
         case when coalesce(e.properties ->> '$referring_domain', '') in ('', '$direct')
                or e.properties ->> '$referring_domain' = e.properties ->> '$host' then '$direct'
              else lower(e.properties ->> '$referring_domain') end as ref,
         nullif(e.properties ->> 'utm_source', '') as utm_source,
         nullif(e.properties ->> 'utm_medium', '') as utm_medium,
         nullif(e.properties ->> 'utm_campaign', '') as utm_campaign
  from pa.events e
  where e.project_id = $1 and e.event = '$pageview' and e."timestamp" >= $2 and e."timestamp" < $3`;

const ENTRIES = `select distinct on (sid) * from pv order by sid, ts`;

const num = (v: unknown) => Number(v ?? 0);

export async function webOverview(db: Db, projectId: string, q: WebQuery): Promise<WebOverview> {
  const params = [projectId, q.from.toISOString(), q.to.toISOString()];
  return db.asProject(projectId, async (c) => {
    const totals = (
      await c.query<{ views: string; visitors: string; sessions: string; bounces: string }>(
        `with pv as (${PAGEVIEWS}), s as (select sid, count(*) as n from pv group by sid)
         select (select count(*) from pv) as views, (select count(distinct person_id) from pv) as visitors,
                (select count(*) from s) as sessions, (select count(*) from s where n = 1) as bounces`,
        params,
      )
    ).rows[0]!;
    const series = await c.query<{ day: string; views: string; visitors: string }>(
      `with pv as (${PAGEVIEWS}),
            days as (select generate_series(date_trunc('day', $2::timestamptz at time zone 'UTC'), date_trunc('day', ($3::timestamptz - interval '1 microsecond') at time zone 'UTC'), interval '1 day') as d)
       select to_char(days.d, 'YYYY-MM-DD') as day, count(pv.ts) as views, count(distinct pv.person_id) as visitors
       from days left join pv on date_trunc('day', pv.ts at time zone 'UTC') = days.d
       group by days.d order by days.d`,
      params,
    );
    const table = async (expr: string, source: "pv" | "entries", where = "true") =>
      (
        await c.query<{ value: string; views: string; visitors: string }>(
          `with pv as (${PAGEVIEWS}), entries as (${ENTRIES})
           select ${expr} as value, count(*) as views, count(distinct person_id) as visitors
           from ${source} where ${where} group by 1 order by 2 desc, 1 limit $4`,
          [...params, q.limit],
        )
      ).rows.map((r) => ({ value: r.value, views: num(r.views), visitors: num(r.visitors) }));
    const pages = await table("path", "pv");
    const referrers = await table("ref", "entries");
    const utm = {
      source: await table("utm_source", "entries", "utm_source is not null"),
      medium: await table("utm_medium", "entries", "utm_medium is not null"),
      campaign: await table("utm_campaign", "entries", "utm_campaign is not null"),
    };
    const names = Object.keys(VITAL_THRESHOLDS) as VitalName[];
    const vr = (
      await c.query<Record<string, string | number | null>>(
        `select ${names
          .map((n) => {
            const v = `case when (e.properties ->> '$web_vitals_${n}_value') ~ '^[0-9]+(\\.[0-9]+)?([eE]-?[0-9]+)?$' then (e.properties ->> '$web_vitals_${n}_value')::float8 end`;
            return `percentile_cont(0.75) within group (order by ${v}) as ${n.toLowerCase()}_p75, count(${v}) as ${n.toLowerCase()}_n`;
          })
          .join(", ")}
         from pa.events e
         where e.project_id = $1 and e.event = '$web_vitals' and e."timestamp" >= $2 and e."timestamp" < $3`,
        params,
      )
    ).rows[0]!;
    const vitals = Object.fromEntries(
      names.map((n) => {
        const p75 = vr[`${n.toLowerCase()}_p75`] === null ? null : Number(vr[`${n.toLowerCase()}_p75`]);
        const [good, poor] = VITAL_THRESHOLDS[n];
        const rating = p75 === null ? null : p75 <= good ? "good" : p75 < poor ? "needs_improvement" : "poor";
        return [n, { p75: p75 === null ? null : Math.round(p75 * 10_000) / 10_000, samples: num(vr[`${n.toLowerCase()}_n`]), rating }];
      }),
    ) as Record<VitalName, Vital>;
    const sessions = num(totals.sessions);
    return {
      range: { from: q.from.toISOString(), to: q.to.toISOString() },
      totals: { visitors: num(totals.visitors), views: num(totals.views), sessions, bounce_rate: sessions ? num(totals.bounces) / sessions : null },
      series: series.rows.map((r) => ({ day: r.day, views: num(r.views), visitors: num(r.visitors) })),
      pages,
      referrers,
      utm,
      vitals,
    };
  });
}
