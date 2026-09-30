/**
 * Railyard Insights from a server (Wave P PY3.2).
 *
 *   const insights = createInsightsServer({
 *     site: process.env.RAILYARD_SITE_KEY, collector: process.env.RAILYARD_COLLECTOR_URL,   // to send
 *     token: process.env.RAILYARD_TOKEN,                                                    // to read and delete
 *   });
 *   await insights.identify(user.id, { plan: 'pro' });
 *   await insights.track(user.id, 'invoice_paid', { amount: 49 });
 *   const pro = await insights.cohorts.save('Active pro', { performed: { event: 'created_project', min_times: 2, within_days: 30 }, trait: { key: 'plan', equals: 'pro' } });
 *   const csv = await insights.exportEvents({ format: 'csv', from, to });
 *   await insights.deleteUser(user.id);   // done at once, or within 24 hours
 *
 * Sending goes to the edge collector (the site key is public); every server
 * event names the person, because a server has no visitor of its own. Reading,
 * exporting and deleting go to the Railyard console with the org token from
 * `railyard login` (keep it in a secret store; this package never prints it).
 */

type Fetch = (url: string, init?: { method?: string; headers?: Record<string, string>; body?: string }) => Promise<{
  status: number;
  ok: boolean;
  headers: { get(name: string): string | null };
  json(): Promise<unknown>;
  text(): Promise<string>;
}>;

export type Value = string | number | boolean;

export interface InsightsServerOptions {
  site?: string;
  collector?: string;
  /** The org token (`ry_...`) for reads, exports and deletes. */
  token?: string;
  console?: string;
  /** The app name or id to read by default. */
  app?: string;
  fetch?: Fetch;
  now?: () => number;
}

export class InsightsError extends Error {
  status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

export interface CohortDefinition {
  app_id?: string | null;
  performed?: { event: string; min_times?: number; within_days: number } | null;
  not_performed?: { event: string; within_days: number } | null;
  trait?: { key: string; equals: Value } | null;
  first_seen_within_days?: number | null;
}

const SITE = /^ry_site_[A-Za-z0-9_-]{4,90}$/;
const USER_ID = /^[A-Za-z0-9@._:+=-]{1,50}$/;
const TOKEN = /^ry_[A-Za-z0-9_-]{20,}$/;

function clean(data?: Record<string, unknown>): Record<string, Value> | undefined {
  if (!data) return undefined;
  const out: Record<string, Value> = {};
  for (const [k, v] of Object.entries(data)) {
    if (Object.keys(out).length >= 20 || k.length > 50) continue;
    if (typeof v === 'string') out[k] = v.slice(0, 500);
    else if ((typeof v === 'number' && Number.isFinite(v)) || typeof v === 'boolean') out[k] = v;
  }
  return Object.keys(out).length ? out : undefined;
}

function randomId(): string {
  const b = new Uint8Array(16);
  globalThis.crypto.getRandomValues(b);
  return Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
}

export function createInsightsServer(options: InsightsServerOptions) {
  const doFetch: Fetch = options.fetch ?? ((url, init) => globalThis.fetch(url, init) as never);
  const consoleUrl = (options.console ?? 'https://railyard.ereboslab.com').replace(/\/+$/, '');

  async function send(type: 'event' | 'identify', userId: string, payload: Record<string, unknown>) {
    if (!options.site || !SITE.test(options.site)) throw new InsightsError('Set site (the ry_site_ key) to send events.', 400);
    if (!options.collector || !/^https?:\/\//.test(options.collector)) throw new InsightsError('Set collector (the collector URL) to send events.', 400);
    if (!USER_ID.test(userId)) throw new InsightsError('The user id must be 1 to 50 of A-Z a-z 0-9 @ . _ : + = -', 400);
    const body = JSON.stringify([{ type, payload: { website: options.site, hostname: 'server', url: '/', user: userId, id: randomId(), ...payload } }]);
    // Up to three tries; the event id makes a retried delivery count once.
    let last = 0;
    for (let attempt = 0; attempt < 3; attempt += 1) {
      try {
        const res = await doFetch(`${options.collector.replace(/\/+$/, '')}/api/batch`, {
          method: 'POST',
          headers: { 'content-type': 'application/json', 'user-agent': 'Mozilla/5.0 (compatible; RailyardInsightsServer/0.1)' },
          body,
        });
        if (res.status >= 200 && res.status < 300) return;
        last = res.status;
        if (res.status < 500) break;
      } catch {
        last = 0;
      }
    }
    throw new InsightsError(last ? `The collector answered ${last}.` : 'The collector could not be reached.', last || 503);
  }

  async function call<T>(method: string, path: string, query: Record<string, string | number | undefined | null> = {}, body?: unknown, raw = false): Promise<T> {
    if (!options.token || !TOKEN.test(options.token)) throw new InsightsError('Set token (the ry_ org token from railyard login) to read, export or delete.', 401);
    const q = new URLSearchParams();
    const app = query.app ?? options.app;
    for (const [k, v] of Object.entries({ ...query, app })) if (v !== undefined && v !== null && v !== '') q.set(k, String(v));
    const res = await doFetch(`${consoleUrl}/api/v1/insights/${path}${q.size ? `?${q}` : ''}`, {
      method,
      headers: { authorization: `Bearer ${options.token}`, accept: 'application/json', ...(body !== undefined ? { 'content-type': 'application/json' } : {}) },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!res.ok) {
      const j = (await res.json().catch(() => ({}))) as { error?: string };
      throw new InsightsError(j.error ?? `Railyard answered ${res.status}.`, res.status);
    }
    return (raw ? await res.text() : await res.json()) as T;
  }

  return {
    /** Names a person and merges traits into their profile. */
    identify: (userId: string, traits?: Record<string, unknown>) => {
      const data = clean(traits);
      return send('identify', userId, data ? { data } : {});
    },
    /** A custom event done by a person (server events always name the person). */
    track: (userId: string, name: string, data?: Record<string, unknown>) => {
      const d = clean(data);
      return send('event', userId, { name: name.slice(0, 50), ...(d ? { data: d } : {}) });
    },
    profiles: (q: { user?: string; limit?: number; offset?: number; app?: string } = {}) => call<{ profiles: unknown[]; events?: unknown[] }>('GET', 'profiles', q),
    cohorts: {
      list: () => call<{ cohorts: unknown[] }>('GET', 'cohorts'),
      save: (name: string, definition: CohortDefinition) => call<{ cohort_id: string }>('POST', 'cohorts', {}, { name, definition }),
      members: (cohortId: string, limit?: number) => call<{ members: number; people: unknown[] }>('GET', `cohorts/${encodeURIComponent(cohortId)}`, { limit }),
      remove: (cohortId: string) => call<{ deleted: boolean }>('DELETE', `cohorts/${encodeURIComponent(cohortId)}`),
    },
    retention: (q: { property?: string; period?: 'day' | 'week'; periods?: number; days?: number; event?: string; app?: string } = {}) => call<{ rows: unknown[] }>('GET', 'retention', q),
    breakdown: (q: { dimension: string; days?: number; event?: string; limit?: number; app?: string }) => call<{ rows: unknown[] }>('GET', 'breakdown', q),
    /** Events of up to 31 days (JSON rows, or the CSV text). */
    exportEvents: (q: { format?: 'json' | 'csv'; from?: string; to?: string; app?: string } = {}) =>
      q.format === 'csv' ? call<string>('GET', 'export', { ...q, kind: 'events' }, undefined, true) : call<{ rows: unknown[]; truncated: boolean }>('GET', 'export', { ...q, kind: 'events' }),
    exportProfiles: (q: { format?: 'json' | 'csv'; app?: string } = {}) =>
      q.format === 'csv' ? call<string>('GET', 'export', { ...q, kind: 'profiles' }, undefined, true) : call<{ rows: unknown[] }>('GET', 'export', { ...q, kind: 'profiles' }),
    /** Deletes everything about one person (done at once, or within 24 hours). */
    deleteUser: (userId: string, q: { app?: string } = {}) => {
      if (!USER_ID.test(userId)) return Promise.reject(new InsightsError('The user id must be 1 to 50 of A-Z a-z 0-9 @ . _ : + = -', 400));
      return call<{ request_id: string; status: 'done' | 'pending'; due_by: string }>('DELETE', `users/${encodeURIComponent(userId)}`, q);
    },
  };
}
