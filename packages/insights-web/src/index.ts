/**
 * Railyard Insights for web apps (Wave P PY3.3): page views (including
 * client-side route changes), custom events, identify, and the five
 * page-speed metrics of each page view, sent once when the page is hidden or
 * the route changes. No cookies, no storage; nothing is sent when Do Not
 * Track is on or consent is refused.
 *
 *   import { createInsightsWeb } from '@railyard/insights-web';
 *   const insights = createInsightsWeb({ site: 'ry_site_...', collector: 'https://collect.example' });
 *   insights.track('signup');
 *   insights.identify(user.id, { plan: 'pro' });
 */
import { computeVitals, type PerfEntry, type Vitals } from './metrics.ts';

export { computeVitals, p75, type PerfEntry, type Vitals } from './metrics.ts';

export type Value = string | number | boolean;

interface ObserverLike {
  observe(opts: Record<string, unknown>): void;
  takeRecords?(): PerfEntry[];
}

/** The browser pieces used; tests pass their own. */
export interface WebEnv {
  location: { pathname: string; search: string; hostname: string };
  document: { referrer: string; title: string; visibilityState?: string; addEventListener(type: string, fn: () => void): void };
  navigator: { doNotTrack?: string | null; language?: string; sendBeacon?(url: string, data: Blob | string): boolean };
  screen?: { width: number; height: number };
  history?: { pushState(...args: unknown[]): void; replaceState?(...args: unknown[]): void };
  addEventListener(type: string, fn: () => void): void;
  fetch?(url: string, init: Record<string, unknown>): Promise<unknown>;
  PerformanceObserver?: {
    new (cb: (list: { getEntries(): PerfEntry[] }) => void): ObserverLike;
    supportedEntryTypes?: readonly string[];
  };
  performance?: { interactionCount?: number };
  Blob?: new (parts: string[], opts: { type: string }) => Blob;
}

export interface InsightsWebOptions {
  site: string;
  collector: string;
  consent?: () => boolean;
  /** Send page views automatically (default true). */
  pageViews?: boolean;
  /** Measure page speed (default true). */
  vitals?: boolean;
  env?: WebEnv;
}

const SITE = /^ry_site_[A-Za-z0-9_-]{4,90}$/;
const USER_ID = /^[A-Za-z0-9@._:+=-]{1,50}$/;
const OBSERVED = ['navigation', 'paint', 'largest-contentful-paint', 'layout-shift', 'event', 'first-input'];

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

export function createInsightsWeb(options: InsightsWebOptions) {
  if (!SITE.test(options.site ?? '')) throw new Error('createInsightsWeb needs the site key (ry_site_...)');
  const base = options.collector.replace(/\/+$/, '');
  if (!/^https?:\/\//.test(base)) throw new Error('createInsightsWeb needs the collector URL');
  const env = options.env ?? (globalThis as unknown as WebEnv);
  const endpoint = `${base}/api/send`;
  const allowed = () => env.navigator.doNotTrack !== '1' && (options.consent ? options.consent() : true);
  let user: string | null = null;
  let entries: PerfEntry[] = [];
  let vitalsSent = false;
  let lastUrl: string | null = null;

  const payload = (url = env.location.pathname + env.location.search) => ({
    website: options.site,
    url,
    hostname: env.location.hostname,
    referrer: env.document.referrer || null,
    title: env.document.title,
    language: env.navigator.language,
    ...(env.screen ? { screen: `${env.screen.width}x${env.screen.height}` } : {}),
    ...(user ? { user } : {}),
  });

  const post = (type: 'event' | 'identify' | 'performance', p: Record<string, unknown>) => {
    if (!allowed()) return;
    const body = JSON.stringify({ type, payload: p });
    if (env.navigator.sendBeacon && env.Blob && env.navigator.sendBeacon(endpoint, new env.Blob([body], { type: 'text/plain' }))) return;
    void env.fetch?.(endpoint, { method: 'POST', body, keepalive: true, headers: { 'content-type': 'text/plain' } })?.catch?.(() => {});
  };

  const sendVitals = () => {
    if (vitalsSent || options.vitals === false || !env.PerformanceObserver) return;
    const supported = env.PerformanceObserver.supportedEntryTypes ?? [];
    const v: Vitals = computeVitals(entries, { interactionCount: env.performance?.interactionCount, layoutShiftSupported: supported.includes('layout-shift') });
    vitalsSent = true;
    // The metrics belong to the page view they were measured on, even after a route change.
    if (Object.keys(v).length) post('performance', { ...payload(lastUrl ?? undefined), ...v });
  };

  const page = () => {
    const url = env.location.pathname + env.location.search;
    if (url === lastUrl) return;
    if (lastUrl !== null) {
      // A client-side route change: the previous page view's metrics go first.
      sendVitals();
      entries = [];
      vitalsSent = false;
    }
    lastUrl = url;
    if (options.pageViews !== false) post('event', payload());
  };

  if (options.vitals !== false && env.PerformanceObserver) {
    const supported = env.PerformanceObserver.supportedEntryTypes ?? OBSERVED;
    for (const type of OBSERVED.filter((t) => supported.includes(t))) {
      try {
        const obs = new env.PerformanceObserver((list) => {
          entries = [...entries, ...list.getEntries()];
        });
        obs.observe(type === 'event' ? { type, buffered: true, durationThreshold: 40 } : { type, buffered: true });
      } catch {
        // Not supported in this browser.
      }
    }
    env.document.addEventListener('visibilitychange', () => {
      if (env.document.visibilityState === 'hidden') sendVitals();
    });
    env.addEventListener('pagehide', sendVitals);
  }

  if (env.history) {
    const push = env.history.pushState.bind(env.history);
    env.history.pushState = (...args: unknown[]) => {
      push(...args);
      page();
    };
  }
  env.addEventListener('popstate', page);
  page();

  return {
    track(name: string, data?: Record<string, unknown>) {
      const d = clean(data);
      post('event', { ...payload(), name: String(name).slice(0, 50), ...(d ? { data: d } : {}) });
    },
    identify(userId: string, traits?: Record<string, unknown>) {
      if (!USER_ID.test(userId)) throw new Error('identify needs the app user id: 1 to 50 of A-Z a-z 0-9 @ . _ : + = -');
      user = userId;
      const d = clean(traits);
      post('identify', { ...payload(), ...(d ? { data: d } : {}) });
    },
    reset() {
      user = null;
    },
    /** Sends this page view's metrics now (normally done when the page is hidden). */
    flushVitals: sendVitals,
  };
}
