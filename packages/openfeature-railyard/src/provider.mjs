// OpenFeature providers for Railyard Flags (Wave P PY2.1). They follow the
// OpenFeature provider interface without depending on an SDK package: pass
// `events: new OpenFeatureEventEmitter()` from the SDK you use to get
// configuration-changed events on the client.
//
//   import { OpenFeature, OpenFeatureEventEmitter } from "@openfeature/web-sdk";
//   await OpenFeature.setProviderAndWait(new RailyardFlagsProvider({ key, events: new OpenFeatureEventEmitter() }));
//
// A flag is a boolean (enabled) with an optional text value; the string,
// number and object resolvers read the value (the object one parses JSON).

import { createFlagsClient, DEFAULT_URL } from "./client.mjs";

const CONFIGURATION_CHANGED = "PROVIDER_CONFIGURATION_CHANGED";

/** @param {ReturnType<typeof createFlagsClient> | { all(): Record<string, {enabled: boolean, value: string | null}> }} source */
function resolver(read) {
  const miss = (value) => ({ value, reason: "DEFAULT", errorCode: "FLAG_NOT_FOUND" });
  return {
    boolean(key, fallback) {
      const f = read()[key];
      return f ? { value: f.enabled, reason: "STATIC", variant: f.enabled ? "on" : "off" } : miss(fallback);
    },
    string(key, fallback) {
      const f = read()[key];
      if (!f) return miss(fallback);
      return f.enabled && f.value !== null ? { value: f.value, reason: "STATIC" } : { value: fallback, reason: "DISABLED" };
    },
    number(key, fallback) {
      const f = read()[key];
      if (!f) return miss(fallback);
      const n = f.value === null ? NaN : Number(f.value);
      if (!f.enabled) return { value: fallback, reason: "DISABLED" };
      return Number.isFinite(n) ? { value: n, reason: "STATIC" } : { value: fallback, reason: "ERROR", errorCode: "TYPE_MISMATCH" };
    },
    object(key, fallback) {
      const f = read()[key];
      if (!f) return miss(fallback);
      if (!f.enabled || f.value === null) return { value: fallback, reason: "DISABLED" };
      try {
        return { value: JSON.parse(f.value), reason: "STATIC" };
      } catch {
        return { value: fallback, reason: "ERROR", errorCode: "PARSE_ERROR" };
      }
    },
  };
}

/** Client provider (browser, Expo): one polling client, synchronous resolution. */
export class RailyardFlagsProvider {
  /** @param {Parameters<typeof createFlagsClient>[0] & { events?: { emit(event: string, details?: unknown): void } }} options */
  constructor(options) {
    this.metadata = { name: "railyard-flags" };
    this.runsOn = "client";
    this.events = options.events;
    this.client = createFlagsClient(options);
    this.unsubscribe = this.client.subscribe(() => this.events?.emit(CONFIGURATION_CHANGED, { providerName: this.metadata.name }));
    const r = resolver(() => this.client.all());
    this.resolveBooleanEvaluation = (key, fallback) => r.boolean(key, fallback);
    this.resolveStringEvaluation = (key, fallback) => r.string(key, fallback);
    this.resolveNumberEvaluation = (key, fallback) => r.number(key, fallback);
    this.resolveObjectEvaluation = (key, fallback) => r.object(key, fallback);
  }
  async initialize() {
    await this.client.ready();
  }
  async onClose() {
    this.unsubscribe();
    this.client.close();
  }
}

/**
 * Server provider (Next.js route handlers and server components): reads on
 * demand and keeps the answer `ttlMs` (15 s by default), so every request sees
 * a flip within the same 60 s as the apps.
 */
export class RailyardServerFlagsProvider {
  /** @param {{ key: string, url?: string, ttlMs?: number, fetch?: typeof fetch, now?: () => number }} options */
  constructor(options) {
    this.metadata = { name: "railyard-flags" };
    this.runsOn = "server";
    this.url = `${(options.url ?? DEFAULT_URL).replace(/\/$/, "")}?key=${encodeURIComponent(options.key)}`;
    this.ttlMs = options.ttlMs ?? 15_000;
    this.fetch = options.fetch ?? globalThis.fetch.bind(globalThis);
    this.now = options.now ?? Date.now;
    this.cached = null;
    this.at = 0;
    if (!/^ry_flags_[A-Za-z0-9_-]{24,64}$/.test(options.key ?? "")) throw new Error("RailyardServerFlagsProvider needs the environment's flags key (ry_flags_...)");
    const r = resolver(() => this.cached ?? {});
    const wrap = (fn) => async (key, fallback) => {
      await this.load();
      return fn(key, fallback);
    };
    this.resolveBooleanEvaluation = wrap(r.boolean);
    this.resolveStringEvaluation = wrap(r.string);
    this.resolveNumberEvaluation = wrap(r.number);
    this.resolveObjectEvaluation = wrap(r.object);
  }
  async load() {
    if (this.cached && this.now() - this.at < this.ttlMs) return;
    try {
      const res = await this.fetch(this.url, { headers: { accept: "application/json" } });
      if (!res.ok) throw new Error(String(res.status));
      const body = await res.json();
      const flags = body && typeof body === "object" ? body.flags : null;
      if (flags && typeof flags === "object") {
        this.cached = Object.fromEntries(Object.entries(flags).map(([k, f]) => [k, { enabled: f?.enabled === true, value: typeof f?.value === "string" ? f.value : null }]));
        this.at = this.now();
      }
    } catch {
      // Keep the last answer (or defaults) when the read fails.
    }
  }
  async initialize() {
    await this.load();
  }
}
