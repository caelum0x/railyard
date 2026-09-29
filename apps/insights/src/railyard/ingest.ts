/**
 * Batch ingest from the Railyard edge collector (fork addition; contract:
 * railyard apps/insights-edge/contract/INGEST-API.md, version 1).
 *
 * `POST /v1/ingest/batch` with `authorization: Bearer <INSIGHTS_INGEST_TOKEN>`.
 * The collector already checked the site key, the plan cap and bots, and it
 * never sends an IP address; this side never adds one. A retried request is
 * de-duplicated by (org_id, sent_at, site_key). Pure: the storage functions are
 * passed in, so the whole path is tested without a database.
 */

// Kept import-free so node:test runs these modules without the app's build.
const isUuid = (v: unknown): v is string =>
  typeof v === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(v);

export const MAX_EVENTS_PER_SITE = 10_000;
export const EVENT_TYPE_PAGE_VIEW = 1;
export const EVENT_TYPE_CUSTOM = 2;

export interface EdgeEvent {
  type: 'event';
  name: string | null;
  url: string;
  hostname: string | null;
  referrer: string | null;
  title: string | null;
  language: string | null;
  screen: string | null;
  data: Record<string, string | number | boolean> | null;
  visitor: string;
  country: string | null;
  ts: string;
}

export interface EdgeBatch {
  site_key: string;
  app_id: string;
  events: EdgeEvent[];
}

export interface IngestBody {
  org_id: string;
  sent_at: string;
  batches: EdgeBatch[];
}

const str = (v: unknown, max: number): string | null => (typeof v === 'string' && v.length <= max ? v : null);

function parseEvent(v: unknown): EdgeEvent | null {
  if (!v || typeof v !== 'object') return null;
  const e = v as Record<string, unknown>;
  const url = str(e.url, 2000);
  const visitor = str(e.visitor, 64);
  const ts = str(e.ts, 40);
  if (e.type !== 'event' || url === null || !visitor || !/^[0-9a-f]{16,64}$/.test(visitor) || !ts || Number.isNaN(Date.parse(ts))) return null;
  let data: EdgeEvent['data'] = null;
  if (e.data && typeof e.data === 'object' && !Array.isArray(e.data)) {
    const entries = Object.entries(e.data as Record<string, unknown>).filter(
      ([k, x]) => k.length <= 50 && (typeof x === 'number' || typeof x === 'boolean' || (typeof x === 'string' && x.length <= 500)),
    );
    if (entries.length > 20) return null;
    data = Object.fromEntries(entries) as EdgeEvent['data'];
  }
  const name = e.name === null || e.name === undefined ? null : str(e.name, 50);
  if (e.name !== null && e.name !== undefined && name === null) return null;
  return {
    type: 'event',
    name,
    url,
    hostname: str(e.hostname, 100),
    referrer: str(e.referrer, 2000),
    title: str(e.title, 500),
    language: str(e.language, 35),
    screen: str(e.screen, 11),
    data,
    visitor,
    country: typeof e.country === 'string' && /^[A-Z]{2}$/.test(e.country) ? e.country : null,
    ts,
  };
}

/** Validates the collector's body; a string says what is wrong. */
export function parseIngest(body: unknown): IngestBody | string {
  if (!body || typeof body !== 'object') return 'body must be a JSON object';
  const b = body as Record<string, unknown>;
  if (!isUuid(b.org_id)) return 'org_id must be a UUID';
  const sentAt = str(b.sent_at, 40);
  if (!sentAt || Number.isNaN(Date.parse(sentAt))) return 'sent_at must be a timestamp';
  if (!Array.isArray(b.batches) || b.batches.length === 0 || b.batches.length > 1000) return 'batches must be a non-empty array';
  const batches: EdgeBatch[] = [];
  for (const raw of b.batches) {
    const x = raw as Record<string, unknown>;
    const siteKey = str(x?.site_key, 100);
    if (!siteKey || !/^ry_site_[A-Za-z0-9_-]{8,}$/.test(siteKey)) return 'site_key is malformed';
    if (!isUuid(x.app_id)) return 'app_id must be a UUID';
    if (!Array.isArray(x.events) || x.events.length > MAX_EVENTS_PER_SITE) return 'events must be an array of at most 10000';
    const events = x.events.map(parseEvent);
    if (events.some((e) => e === null)) return 'an event is malformed';
    batches.push({ site_key: siteKey, app_id: x.app_id, events: events as EdgeEvent[] });
  }
  return { org_id: b.org_id as string, sent_at: sentAt, batches };
}

export interface SessionRow {
  id: string;
  websiteId: string;
  screen: string | null;
  language: string | null;
  country: string | null;
  createdAt: Date;
}

export interface EventRow {
  websiteId: string;
  sessionId: string;
  visitId: string;
  eventType: number;
  createdAt: Date;
  pageTitle: string | null;
  hostname: string | null;
  urlPath: string;
  urlQuery: string;
  referrerPath: string | null;
  referrerQuery: string | null;
  referrerDomain: string | null;
  screen: string | null;
  language: string | null;
  country: string | null;
  eventName: string | null;
  eventData: Record<string, string | number | boolean> | null;
}

/** Maps one collector event to upstream rows. `id` derives stable UUIDs (upstream's v5 hash). */
export function toRows(appId: string, e: EdgeEvent, id: (...parts: string[]) => string): { session: SessionRow; event: EventRow } {
  const createdAt = new Date(e.ts);
  const day = e.ts.slice(0, 10);
  const hour = e.ts.slice(0, 13);
  const sessionId = id(appId, e.visitor, day);
  const base = e.hostname ? `https://${e.hostname}` : 'https://localhost';
  const current = new URL(e.url, base);
  let referrerPath: string | null = null;
  let referrerQuery: string | null = null;
  let referrerDomain: string | null = null;
  if (e.referrer) {
    try {
      const r = new URL(e.referrer, base);
      referrerPath = r.pathname;
      referrerQuery = r.search.slice(1);
      const domain = r.hostname.replace(/^www\./, '');
      referrerDomain = domain === current.hostname.replace(/^www\./, '') ? null : domain;
    } catch {
      referrerDomain = null;
    }
  }
  return {
    session: { id: sessionId, websiteId: appId, screen: e.screen, language: e.language, country: e.country, createdAt },
    event: {
      websiteId: appId,
      sessionId,
      visitId: id(sessionId, hour),
      eventType: e.name ? EVENT_TYPE_CUSTOM : EVENT_TYPE_PAGE_VIEW,
      createdAt,
      pageTitle: e.title,
      hostname: e.hostname ?? current.hostname,
      urlPath: current.pathname + current.hash,
      urlQuery: current.search.slice(1),
      referrerPath,
      referrerQuery,
      referrerDomain,
      screen: e.screen,
      language: e.language,
      country: e.country,
      eventName: e.name,
      eventData: e.data,
    },
  };
}

export interface IngestDeps {
  /** True the first time (org, sent_at, site_key) is seen. */
  receipt(orgId: string, sentAt: string, siteKey: string): Promise<boolean>;
  ensureWebsite(orgId: string, appId: string, siteKey: string, hostname: string | null): Promise<boolean>;
  createSession(row: SessionRow): Promise<void>;
  saveEvent(row: EventRow): Promise<void>;
  id(...parts: string[]): string;
}

export interface IngestResult {
  status: number;
  body: { stored: number; duplicate_batches: number } | { error: string };
}

/** Stores a validated body. Idempotent per (org, sent_at, site). */
export async function ingest(deps: IngestDeps, body: IngestBody): Promise<IngestResult> {
  let stored = 0;
  let duplicates = 0;
  for (const batch of body.batches) {
    const host = batch.events.find((e) => e.hostname)?.hostname ?? null;
    if (!(await deps.ensureWebsite(body.org_id, batch.app_id, batch.site_key, host))) {
      return { status: 409, body: { error: `app ${batch.app_id} belongs to another org` } };
    }
    if (!(await deps.receipt(body.org_id, body.sent_at, batch.site_key))) {
      duplicates += 1;
      continue;
    }
    const sessions = new Set<string>();
    for (const e of batch.events) {
      const rows = toRows(batch.app_id, e, deps.id);
      if (!sessions.has(rows.session.id)) {
        sessions.add(rows.session.id);
        await deps.createSession(rows.session);
      }
      await deps.saveEvent(rows.event);
      stored += 1;
    }
  }
  return { status: 200, body: { stored, duplicate_batches: duplicates } };
}

/** Constant-time bearer check against one or two accepted tokens (rotation). */
export function bearerOk(header: string | null, tokens: readonly (string | undefined)[]): boolean {
  const m = /^Bearer\s+(\S{16,})$/.exec(header ?? '');
  if (!m) return false;
  const given = m[1] as string;
  return tokens.some((t) => {
    if (!t || t.length !== given.length) return false;
    let diff = 0;
    for (let i = 0; i < t.length; i += 1) diff |= t.charCodeAt(i) ^ given.charCodeAt(i);
    return diff === 0;
  });
}
