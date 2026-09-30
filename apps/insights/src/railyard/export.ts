/**
 * CSV and JSON export (fork addition, PY3.2): raw events or profiles of one
 * org, for a window of at most 31 days and at most 100,000 rows per file.
 * CSV cells that a spreadsheet would run as a formula are prefixed with a
 * quote, so an exported URL or trait can never execute.
 */

// Kept import-free so node:test runs these modules without the app's build.
const isUuid = (v: unknown): v is string =>
  typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v);

export const MAX_EXPORT_ROWS = 100_000;
export const MAX_EXPORT_DAYS = 31;

export const EXPORT_EVENTS_SQL = `
  select e.created_at, e.website_id::text as app_id, e.event_type, e.event_name, e.url_path, e.url_query,
         e.referrer_domain, e.page_title, e.hostname, s.country, s.language, s.screen,
         e.session_id::text as session_id, s.distinct_id as user_id
  from website_event e
  join website w on w.website_id = e.website_id
  left join session s on s.session_id = e.session_id and s.website_id = e.website_id
  where w.team_id = {{orgId::uuid}} and w.deleted_at is null
    and ({{appId}}::uuid is null or e.website_id = {{appId}}::uuid)
    and e.created_at >= {{from::timestamptz}} and e.created_at < {{to::timestamptz}}
  order by e.created_at, e.event_id
  limit {{limit::int}}`;

export const EXPORT_PROFILES_SQL = `
  select p.website_id::text as app_id, p.distinct_id as user_id, p.first_seen, p.last_seen, p.traits
  from railyard_profile p
  join website w on w.website_id = p.website_id
  where w.team_id = {{orgId::uuid}} and w.deleted_at is null
    and ({{appId}}::uuid is null or p.website_id = {{appId}}::uuid)
  order by p.first_seen, p.distinct_id
  limit {{limit::int}}`;

export const EVENT_COLUMNS = ['time', 'app_id', 'type', 'name', 'path', 'query', 'referrer', 'title', 'hostname', 'country', 'language', 'screen', 'session_id', 'user_id'] as const;
export const PROFILE_COLUMNS = ['app_id', 'user_id', 'first_seen', 'last_seen', 'traits'] as const;

export interface Query {
  query<T>(sql: string, data: Record<string, unknown>): Promise<T[]>;
}

/** One CSV cell: quoted when needed; a leading =, +, -, @, tab or CR gets a leading quote. */
export function csvCell(v: unknown): string {
  if (v === null || v === undefined) return '';
  let s = typeof v === 'object' ? JSON.stringify(v) : String(v);
  if (/^[=+\-@\t\r]/.test(s)) s = `'${s}`;
  return /[",\n\r]/.test(s) ? `"${s.replace(/"/g, '""')}"` : s;
}

export function toCsv(columns: readonly string[], rows: Record<string, unknown>[]): string {
  return [columns.join(','), ...rows.map((r) => columns.map((c) => csvCell(r[c])).join(','))].join('\r\n') + '\r\n';
}

const iso = (v: unknown) => (v === null || v === undefined ? null : new Date(v as string).toISOString());

export async function exportRows(
  db: Query,
  orgId: string,
  q: { kind?: string; app?: string | null; from?: string | null; to?: string | null },
  now: Date,
): Promise<{ kind: 'events' | 'profiles'; columns: readonly string[]; rows: Record<string, unknown>[]; truncated: boolean } | string> {
  if (!isUuid(orgId)) return 'org must be a UUID';
  const appId = q.app ?? null;
  if (appId !== null && !isUuid(appId)) return 'app must be a UUID';
  const kind = q.kind ?? 'events';
  if (kind === 'profiles') {
    const raw = await db.query<Record<string, unknown>>(EXPORT_PROFILES_SQL, { orgId, appId, limit: MAX_EXPORT_ROWS + 1 });
    const rows = raw.slice(0, MAX_EXPORT_ROWS).map((r) => ({
      app_id: r.app_id,
      user_id: r.user_id,
      first_seen: iso(r.first_seen),
      last_seen: iso(r.last_seen),
      traits: typeof r.traits === 'string' ? JSON.parse(r.traits) : r.traits,
    }));
    return { kind, columns: PROFILE_COLUMNS, rows, truncated: raw.length > MAX_EXPORT_ROWS };
  }
  if (kind !== 'events') return 'kind must be events or profiles';
  const to = q.to ? new Date(q.to) : now;
  const from = q.from ? new Date(q.from) : new Date(to.getTime() - 7 * 86_400_000);
  if (Number.isNaN(to.getTime()) || Number.isNaN(from.getTime()) || from >= to) return 'from and to must be times with from before to';
  if (to.getTime() - from.getTime() > MAX_EXPORT_DAYS * 86_400_000) return 'export at most 31 days at a time';
  const raw = await db.query<Record<string, unknown>>(EXPORT_EVENTS_SQL, { orgId, appId, from: from.toISOString(), to: to.toISOString(), limit: MAX_EXPORT_ROWS + 1 });
  const rows = raw.slice(0, MAX_EXPORT_ROWS).map((r) => ({
    time: iso(r.created_at),
    app_id: r.app_id,
    type: Number(r.event_type) === 1 ? 'pageview' : 'event',
    name: r.event_name ?? null,
    path: r.url_path ?? null,
    query: r.url_query ?? null,
    referrer: r.referrer_domain ?? null,
    title: r.page_title ?? null,
    hostname: r.hostname ?? null,
    country: r.country ?? null,
    language: r.language ?? null,
    screen: r.screen ?? null,
    session_id: r.session_id,
    user_id: r.user_id ?? null,
  }));
  return { kind, columns: EVENT_COLUMNS, rows, truncated: raw.length > MAX_EXPORT_ROWS };
}
