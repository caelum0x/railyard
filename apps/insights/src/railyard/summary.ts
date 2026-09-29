/**
 * Per-organization summaries for the Railyard console and the Scribase
 * Analytics tab (fork addition). Every query is scoped by the org through
 * `website.team_id`, so one org can never read another's rows (tested at the
 * SQL level against a scratch Postgres with the upstream migrations).
 *
 * The funnel is the one a new app cares about first: a visit, then a sign-up
 * (custom event `signup` or `sign_up`), then a first action (any other custom
 * event), counted per session over the window.
 */

// Kept import-free so node:test runs these modules without the app's build.
const isUuid = (v: unknown): v is string =>
  typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v);

export const SIGNUP_EVENTS = ['signup', 'sign_up'] as const;

/** Apps of the org with their first event and counts over the window. */
export const APPS_SQL = `
  select w.website_id::text as app_id,
         min(e.created_at) as first_event_at,
         count(e.event_id) filter (where e.created_at >= {{since::timestamptz}}) as events,
         count(distinct e.session_id) filter (where e.created_at >= {{since::timestamptz}}) as visitors,
         count(e.event_id) filter (where e.created_at >= {{monthStart::timestamptz}}) as month_events
  from website w
  left join website_event e on e.website_id = w.website_id
  where w.team_id = {{orgId::uuid}} and w.deleted_at is null
  group by w.website_id
  order by w.website_id`;

/** Visit, sign-up, first action: sessions reaching each step (in order) since the window start. */
export const FUNNEL_SQL = `
  with scoped as (
    select e.session_id, e.event_type, e.event_name, e.created_at
    from website_event e
    join website w on w.website_id = e.website_id
    where w.team_id = {{orgId::uuid}} and w.deleted_at is null
      and ({{appId}}::uuid is null or w.website_id = {{appId}}::uuid)
      and e.created_at >= {{since::timestamptz}}
  ),
  visit as (select session_id, min(created_at) as at from scoped where event_type = 1 group by session_id),
  signup as (
    select s.session_id, min(s.created_at) as at from scoped s join visit v using (session_id)
    where s.event_type = 2 and lower(s.event_name) in ('signup', 'sign_up') and s.created_at >= v.at group by s.session_id
  ),
  action as (
    select s.session_id from scoped s join signup u using (session_id)
    where s.event_type = 2 and lower(s.event_name) not in ('signup', 'sign_up') and s.created_at >= u.at group by s.session_id
  )
  select (select count(*) from visit) as visits,
         (select count(*) from signup) as signups,
         (select count(*) from action) as first_actions`;

export interface AppRow {
  app_id: string;
  first_event_at: Date | string | null;
  events: number | string | bigint;
  visitors: number | string | bigint;
  month_events: number | string | bigint;
}

export interface FunnelRow {
  visits: number | string | bigint;
  signups: number | string | bigint;
  first_actions: number | string | bigint;
}

export interface Summary {
  org_id: string;
  since: string;
  /** Stored events since the start of the UTC month (the plan cap counts these). */
  apps: { app_id: string; first_event_at: string | null; events: number; visitors: number; month_events: number }[];
  funnel: { steps: { name: 'visit' | 'signup' | 'first_action'; sessions: number }[] };
}

export interface SummaryDeps {
  query<T>(sql: string, data: Record<string, unknown>): Promise<T[]>;
}

const n = (v: number | string | bigint): number => Number(v);
const iso = (v: Date | string | null): string | null => (v === null ? null : new Date(v).toISOString());

/** The org's summary, or a string saying what was wrong with the request. */
export async function summarize(deps: SummaryDeps, orgId: string, appId: string | null, days: number, now: Date): Promise<Summary | string> {
  if (!isUuid(orgId)) return 'org must be a UUID';
  if (appId !== null && !isUuid(appId)) return 'app must be a UUID';
  if (!Number.isInteger(days) || days < 1 || days > 90) return 'days must be 1 to 90';
  const since = new Date(now.getTime() - days * 86_400_000).toISOString();
  const monthStart = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), 1)).toISOString();
  const apps = await deps.query<AppRow>(APPS_SQL, { orgId, since, monthStart });
  const [f] = await deps.query<FunnelRow>(FUNNEL_SQL, { orgId, appId, since });
  return {
    org_id: orgId,
    since,
    apps: apps
      .filter((a) => appId === null || a.app_id === appId)
      .map((a) => ({
        app_id: a.app_id,
        first_event_at: iso(a.first_event_at),
        events: n(a.events),
        visitors: n(a.visitors),
        month_events: n(a.month_events),
      })),
    funnel: {
      steps: [
        { name: 'visit', sessions: n(f?.visits ?? 0) },
        { name: 'signup', sessions: n(f?.signups ?? 0) },
        { name: 'first_action', sessions: n(f?.first_actions ?? 0) },
      ],
    },
  };
}
