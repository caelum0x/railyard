/**
 * Railyard Insights for React Native and Expo apps (Wave P PY3.2).
 *
 *   import AsyncStorage from '@react-native-async-storage/async-storage';
 *   const insights = createInsights({ site: 'ry_site_...', collector: 'https://collect.example', storage: AsyncStorage });
 *   insights.screen('/home');
 *   await insights.identify(user.id, { plan: 'pro' });   // after sign-in
 *   insights.track('created_project', { template: 'blank' });
 *   await insights.reset();                               // after sign-out
 *
 * Events go to the edge collector over plain HTTP from a persistent queue:
 * an app that was offline sends everything once it is back, each event once
 * (every event carries an id the collector de-duplicates on). The user id set
 * by identify is kept in storage, so events after a restart still belong to
 * that person, until reset. With consent refused (`consent()` returns false)
 * nothing is queued or sent. No IP address is ever read here.
 */
import { createQueue, defaultFetch, type HttpFetch, type KeyValueStorage } from './queue.ts';

export type { HttpFetch, KeyValueStorage } from './queue.ts';

export type Value = string | number | boolean;

export interface InsightsOptions {
  /** The site key (`ry_site_...`), public like an anon key. */
  site: string;
  /** The collector base URL. */
  collector: string;
  storage?: KeyValueStorage | null;
  fetch?: HttpFetch;
  /** Whether the person agreed to measurement; defaults to allowed. */
  consent?: () => boolean;
  /** Shown as the event hostname (the app's name or bundle id). */
  app?: string;
  platform?: string;
  /** The device language, e.g. from expo-localization. */
  language?: string;
  /** "<width>x<height>" of the screen. */
  screenSize?: string;
  now?: () => number;
  setTimeout?: (fn: () => void, ms: number) => unknown;
  retryMs?: number;
}

export interface Insights {
  screen(path: string, data?: Record<string, Value>): Promise<void>;
  track(name: string, data?: Record<string, Value>): Promise<void>;
  /** Names the signed-in person (the app's own user id) and merges traits into their profile. */
  identify(userId: string, traits?: Record<string, Value>): Promise<void>;
  /** Forgets the user id (sign-out); later events are anonymous. */
  reset(): Promise<void>;
  flush(): Promise<number>;
  pending(): number;
  userId(): string | null;
}

interface Payload {
  website: string;
  url: string;
  hostname: string;
  name?: string;
  data?: Record<string, Value>;
  user?: string;
  language?: string;
  screen?: string;
}

const SITE = /^ry_site_[A-Za-z0-9_-]{4,90}$/;
export const USER_ID = /^[A-Za-z0-9@._:+=-]{1,50}$/;
const MAX_TRAITS = 20;

/** Keeps at most 20 plain values with short keys (the collector's own limits). */
export function cleanData(data: Record<string, unknown> | undefined): Record<string, Value> | undefined {
  if (!data) return undefined;
  const out: Record<string, Value> = {};
  for (const [k, v] of Object.entries(data)) {
    if (Object.keys(out).length >= MAX_TRAITS) break;
    if (k.length > 50) continue;
    if (typeof v === 'string') out[k] = v.slice(0, 500);
    else if ((typeof v === 'number' && Number.isFinite(v)) || typeof v === 'boolean') out[k] = v;
  }
  return Object.keys(out).length ? out : undefined;
}

export function createInsights(options: InsightsOptions): Insights {
  if (!SITE.test(options.site ?? '')) throw new Error('createInsights needs the site key (ry_site_...)');
  const base = options.collector.replace(/\/+$/, '');
  if (!/^https?:\/\//.test(base)) throw new Error('createInsights needs the collector URL');
  const doFetch = options.fetch ?? defaultFetch();
  const allowed = options.consent ?? (() => true);
  const hostname = (options.app ?? 'app').slice(0, 100);
  const userKey = `railyard-insights-user:${options.site.slice(-12)}`;
  // Collectors drop bots by user agent; say what this is (never an HTTP library name).
  const userAgent = `Mozilla/5.0 (compatible; RailyardInsightsRN/0.1; ${options.platform ?? 'mobile'})`;
  let user: string | null = null;
  let userLoaded: Promise<void> | null = null;
  const loadUser = () => {
    userLoaded ??= (async () => {
      try {
        const saved = options.storage ? await options.storage.getItem(userKey) : null;
        if (saved && USER_ID.test(saved) && user === null) user = saved;
      } catch {
        // No stored user: anonymous until identify.
      }
    })();
    return userLoaded;
  };
  const queue = createQueue<{ type: 'event' | 'identify'; payload: Payload }>({
    storage: options.storage,
    key: `railyard-insights:${options.site.slice(-12)}`,
    now: options.now,
    setTimeout: options.setTimeout,
    retryMs: options.retryMs,
    send: async (batch) => {
      const body = batch.map((item) => ({ type: item.body.type, payload: { ...item.body.payload, id: item.id } }));
      const res = await doFetch(`${base}/api/batch`, { method: 'POST', headers: { 'content-type': 'application/json', 'user-agent': userAgent }, body: JSON.stringify(body) });
      return res.status >= 200 && res.status < 300;
    },
  });
  const common = () => ({
    website: options.site,
    hostname,
    ...(options.language ? { language: options.language.slice(0, 35) } : {}),
    ...(options.screenSize && /^\d{1,5}x\d{1,5}$/.test(options.screenSize) ? { screen: options.screenSize } : {}),
  });
  const push = async (type: 'event' | 'identify', payload: Payload) => {
    if (!allowed()) return;
    await loadUser();
    await queue.push({ type, payload: { ...payload, ...(user ? { user } : {}) } });
  };
  return {
    screen: (path, data) => {
      const d = cleanData(data);
      return push('event', { ...common(), url: path.startsWith('/') ? path.slice(0, 1000) : `/${path.slice(0, 999)}`, ...(d ? { data: d } : {}) });
    },
    track: (name, data) => {
      const d = cleanData(data);
      return push('event', { ...common(), url: '/', name: name.slice(0, 50), ...(d ? { data: d } : {}) });
    },
    async identify(userId, traits) {
      if (!USER_ID.test(userId)) throw new Error('identify needs the app user id: 1 to 50 of A-Z a-z 0-9 @ . _ : + = -');
      await loadUser();
      user = userId;
      if (options.storage) await options.storage.setItem(userKey, userId);
      const d = cleanData(traits);
      await push('identify', { ...common(), url: '/', ...(d ? { data: d } : {}) });
    },
    async reset() {
      await loadUser();
      user = null;
      if (options.storage) await options.storage.setItem(userKey, '');
    },
    flush: () => queue.flush(),
    pending: () => queue.size(),
    userId: () => user,
  };
}
