// Backstop for the capture endpoints' one real public, unauthenticated cost:
// resolving a presented token. `CachedSiteDirectory` already caches a *miss*
// by its exact token string, so hammering the SAME unknown token is cheap
// after the first try -- but a client that varies the token on every request
// (trivial: `token = "ry_site_" + random()`) never hits that cache and makes
// this service do one full site-directory round trip (PostgREST, Postgres)
// per request, with no ceiling. This counts only *failed* token lookups
// (a real SDK with a real token essentially never 401s), so it never throttles
// legitimate capture traffic, including the aggregated server-side forwarders
// (Insights-edge's OrgCollector, etc.) whose requests all arrive from a small
// shared pool of infra IPs and must never be blocked by an IP-keyed limiter
// that also sees attack traffic from that same kind of address.
//
// Two independent ceilings, both counting failures only:
//   per key (normally the caller's IP)  -- stops one source from flooding
//   global                              -- stops a slow-and-wide probe that
//                                          never trips any single key

export interface FailureLimiterOptions {
  /** Window length each ceiling resets on. */
  windowMs: number;
  /** Failures allowed from one key inside the window. */
  perKeyMax: number;
  /** Failures allowed in total inside the window, across every key. */
  globalMax: number;
  /** Bound on distinct keys tracked at once (oldest window is cleared, not evicted one by one). */
  maxKeys?: number;
}

interface Window {
  start: number;
  count: number;
}

export class FailureLimiter {
  private readonly windowMs: number;
  private readonly perKeyMax: number;
  private readonly globalMax: number;
  private readonly maxKeys: number;
  private perKey = new Map<string, Window>();
  private global: Window = { start: 0, count: 0 };

  constructor(opts: FailureLimiterOptions) {
    this.windowMs = opts.windowMs;
    this.perKeyMax = opts.perKeyMax;
    this.globalMax = opts.globalMax;
    this.maxKeys = opts.maxKeys ?? 20_000;
  }

  /** True when `key` is already over either ceiling for the current window (a failure is not recorded again). */
  blocked(key: string, now: number): boolean {
    const g = this.global;
    const inGlobalWindow = now - g.start < this.windowMs;
    if (inGlobalWindow && g.count >= this.globalMax) return true;
    const k = this.perKey.get(key);
    return Boolean(k && now - k.start < this.windowMs && k.count >= this.perKeyMax);
  }

  /** Records one failed lookup for `key`. Call only after `blocked` returned false. */
  record(key: string, now: number): void {
    if (now - this.global.start >= this.windowMs) this.global = { start: now, count: 0 };
    this.global.count += 1;
    const cur = this.perKey.get(key);
    if (!cur || now - cur.start >= this.windowMs) {
      if (this.perKey.size >= this.maxKeys) this.perKey.clear();
      this.perKey.set(key, { start: now, count: 1 });
      return;
    }
    cur.count += 1;
  }
}

/** The real visitor/forwarder address Cloudflare attaches at the edge; the only IP this service ever looks at, and only as a rate-limit key -- never stored, never in an event. */
export function clientIp(cfConnectingIp: string | null, forwardedFor: string | null): string {
  if (cfConnectingIp) return cfConnectingIp;
  if (forwardedFor) return forwardedFor.split(",")[0].trim() || "unknown";
  return "unknown";
}
