// Click maps and scroll depth for one page URL, read from stored events:
//  - the browser SDK's `$$heatmap` events carry {url: [{x, y, target_fixed, type}]}
//    with the viewport size on the event; x becomes a share of the viewport
//    width (0.5% buckets) and y a 16 px bucket, so different screen widths
//    line up on one overlay;
//  - scroll depth comes from the `$prev_pageview_*` properties the SDK adds to
//    `$pageleave` and to the next `$pageview` (counted once per page view).
// A person's deletion removes these events with the rest of their events.

import type { Db } from "../db.ts";
import { ApiError } from "../replay/shared/console-guard.ts";

export const HEATMAP_TYPES = ["click", "rageclick", "mousemove", "deadclick"] as const;
export type HeatmapType = (typeof HEATMAP_TYPES)[number];
export const X_BUCKETS = 200;
export const Y_BUCKET_PX = 16;
export const MAX_RANGE_DAYS = 90;

export interface HeatmapQuery {
  url: string;
  type: HeatmapType;
  from: Date;
  to: Date;
  viewportMin: number | null;
  viewportMax: number | null;
}

export interface HeatmapPoint {
  /** Share of the viewport width, 0..1. */
  x: number;
  /** Document y in px (top of its 16 px bucket). */
  y: number;
  fixed: boolean;
  count: number;
}

export interface ScrollDepth {
  views: number;
  /** Estimated page height in px (median of the views that report it), or null. */
  page_height: number | null;
  bands: { percent: number; reached: number; share: number }[];
}

const DAY = 86_400_000;

export function parseHeatmapQuery(q: URLSearchParams, now: Date): HeatmapQuery {
  const url = q.get("url") ?? "";
  if (!/^https?:\/\/\S{1,2000}$/.test(url)) throw new ApiError(400, "Name the page by its full address, starting with https://.");
  const type = (q.get("type") ?? "click") as HeatmapType;
  if (!HEATMAP_TYPES.includes(type)) throw new ApiError(400, "The map type must be click, rageclick, mousemove or deadclick.");
  const to = q.get("to") ? new Date(q.get("to")!) : now;
  const from = q.get("from") ? new Date(q.get("from")!) : new Date(to.getTime() - 7 * DAY);
  if (Number.isNaN(from.getTime()) || Number.isNaN(to.getTime()) || from >= to) throw new ApiError(400, "from and to must be dates, from before to.");
  if (to.getTime() - from.getTime() > MAX_RANGE_DAYS * DAY) throw new ApiError(400, `A map covers at most ${MAX_RANGE_DAYS} days.`);
  const num = (name: string) => {
    const v = q.get(name);
    if (v === null || v === "") return null;
    const n = Number(v);
    if (!Number.isInteger(n) || n < 0 || n > 10_000) throw new ApiError(400, `${name} must be a width in px.`);
    return n;
  };
  return { url, type, from, to, viewportMin: num("viewport_min"), viewportMax: num("viewport_max") };
}

export async function heatmapPoints(db: Db, projectId: string, q: HeatmapQuery): Promise<{ points: HeatmapPoint[]; total: number }> {
  return db.asProject(projectId, async (c) => {
    const r = await c.query<{ xb: number; yb: number; fixed: boolean; n: string }>(
      `select least(${X_BUCKETS}, greatest(0, round((p ->> 'x')::float8 / v.vw * ${X_BUCKETS})))::int as xb,
              (floor((p ->> 'y')::float8 / ${Y_BUCKET_PX}) * ${Y_BUCKET_PX})::int as yb,
              coalesce((p ->> 'target_fixed')::boolean, false) as fixed,
              count(*)::bigint as n
       from pa.events e
       cross join lateral (select case when (e.properties ->> '$viewport_width') ~ '^[0-9]{2,5}(\\.[0-9]+)?$' then (e.properties ->> '$viewport_width')::float8 end as vw) v
       cross join lateral jsonb_each(case when jsonb_typeof(e.properties -> '$heatmap_data') = 'object' then e.properties -> '$heatmap_data' else '{}'::jsonb end) as d(url, pts)
       cross join lateral jsonb_array_elements(case when jsonb_typeof(d.pts) = 'array' then d.pts else '[]'::jsonb end) as p
       where e.project_id = $1 and e.event = '$$heatmap' and e."timestamp" >= $2 and e."timestamp" < $3
         and d.url = $4 and p ->> 'type' = $5
         and jsonb_typeof(p -> 'x') = 'number' and jsonb_typeof(p -> 'y') = 'number'
         and v.vw > 0
         and ($6::int is null or v.vw >= $6)
         and ($7::int is null or v.vw <= $7)
       group by 1, 2, 3
       order by 4 desc
       limit 20000`,
      [projectId, q.from.toISOString(), q.to.toISOString(), q.url, q.type, q.viewportMin, q.viewportMax],
    );
    const points = r.rows.map((x) => ({ x: x.xb / X_BUCKETS, y: x.yb, fixed: x.fixed, count: Number(x.n) }));
    return { points, total: points.reduce((a, p) => a + p.count, 0) };
  });
}

export async function scrollDepth(db: Db, projectId: string, q: HeatmapQuery): Promise<ScrollDepth> {
  const u = new URL(q.url);
  return db.asProject(projectId, async (c) => {
    const r = await c.query<{ pct: number; height: number | null }>(
      `select max(least(1, greatest(0, (e.properties ->> '$prev_pageview_max_scroll_percentage')::float8))) as pct,
              max(case when (e.properties ->> '$prev_pageview_max_content_percentage')::float8 > 0
                       then (e.properties ->> '$prev_pageview_max_content')::float8 / (e.properties ->> '$prev_pageview_max_content_percentage')::float8 end) as height
       from pa.events e
       where e.project_id = $1 and e.event in ('$pageleave', '$pageview') and e."timestamp" >= $2 and e."timestamp" < $3
         and e.properties ->> '$prev_pageview_pathname' = $4
         and coalesce(e.properties ->> '$host', $5) = $5
         and (e.properties ->> '$prev_pageview_max_scroll_percentage') ~ '^[0-9]+(\\.[0-9]+)?([eE]-?[0-9]+)?$'
         and (e.properties ->> '$prev_pageview_max_content') ~ '^[0-9]+(\\.[0-9]+)?$'
         and (e.properties ->> '$prev_pageview_max_content_percentage') ~ '^[0-9]+(\\.[0-9]+)?([eE]-?[0-9]+)?$'
       group by e.distinct_id, coalesce(e.properties ->> '$prev_pageview_id', e.uuid::text)`,
      [projectId, q.from.toISOString(), q.to.toISOString(), u.pathname, u.host],
    );
    const views = r.rows.length;
    const bands = Array.from({ length: 10 }, (_, i) => {
      const percent = (i + 1) * 10;
      const reached = r.rows.filter((x) => x.pct * 100 >= percent - 1e-9).length;
      return { percent, reached, share: views ? reached / views : 0 };
    });
    const heights = r.rows.map((x) => x.height).filter((h): h is number => h !== null && h > 0).sort((a, b) => a - b);
    const page_height = heights.length ? Math.round(heights[Math.floor((heights.length - 1) / 2)]) : null;
    return { views, page_height, bands };
  });
}

/** Pages with click data in the last `days` days, most clicked first. */
export async function heatmapUrls(db: Db, projectId: string, now: Date, days = 30): Promise<{ url: string; clicks: number }[]> {
  return db.asProject(projectId, async (c) => {
    const r = await c.query<{ url: string; n: string }>(
      `select d.url, sum(jsonb_array_length(case when jsonb_typeof(d.pts) = 'array' then d.pts else '[]'::jsonb end))::bigint as n
       from pa.events e
       cross join lateral jsonb_each(case when jsonb_typeof(e.properties -> '$heatmap_data') = 'object' then e.properties -> '$heatmap_data' else '{}'::jsonb end) as d(url, pts)
       where e.project_id = $1 and e.event = '$$heatmap' and e."timestamp" >= $2
       group by 1 order by 2 desc limit 50`,
      [projectId, new Date(now.getTime() - days * DAY).toISOString()],
    );
    return r.rows.map((x) => ({ url: x.url, clicks: Number(x.n) }));
  });
}
