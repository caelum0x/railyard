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
//
// Targeting (Wave Q QY1.3): the evaluation context's `targetingKey` is the
// person's id, its other attributes are the person's properties, and
// `cohorts` (a list of cohort ids) the cohorts the person is in. A targeted
// flag answers per person with the server's rules and hash, locally.

import { createFlagsClient, DEFAULT_URL } from "./client.mjs";
import { evaluateAnswer } from "./targeting.mjs";

const CONFIGURATION_CHANGED = "PROVIDER_CONFIGURATION_CHANGED";

/** The OpenFeature evaluation context as a targeting context. */
export function targetingContext(context) {
  if (!context || typeof context !== "object") return {};
  const { targetingKey, cohorts, ...rest } = context;
  return {
    id: typeof targetingKey === "string" || typeof targetingKey === "number" ? String(targetingKey) : null,
    properties: rest,
    cohorts: Array.isArray(cohorts) ? cohorts.filter((c) => typeof c === "string") : [],
  };
}

const REASONS = { STATIC: "STATIC", TARGETING_MATCH: "TARGETING_MATCH", NO_MATCH: "DEFAULT", DISABLED: "DISABLED" };

/** @param {() => Record<string, any>} read @param {() => Record<string, any>} segments */
function resolver(read, segments = () => ({})) {
  const miss = (value) => ({ value, reason: "DEFAULT", errorCode: "FLAG_NOT_FOUND" });
  const run = (key, context) => {
    const f = read()[key];
    return f ? evaluateAnswer(key, f, targetingContext(context), segments()) : null;
  };
  return {
    boolean(key, fallback, context) {
      const r = run(key, context);
      return r ? { value: r.enabled, reason: REASONS[r.reason], variant: r.enabled ? "on" : "off" } : miss(fallback);
    },
    string(key, fallback, context) {
      const r = run(key, context);
      if (!r) return miss(fallback);
      return r.enabled && r.value !== null ? { value: r.value, reason: REASONS[r.reason] } : { value: fallback, reason: r.enabled ? "DEFAULT" : REASONS[r.reason] };
    },
    number(key, fallback, context) {
      const r = run(key, context);
      if (!r) return miss(fallback);
      if (!r.enabled) return { value: fallback, reason: REASONS[r.reason] };
      const n = r.value === null ? NaN : Number(r.value);
      return Number.isFinite(n) ? { value: n, reason: REASONS[r.reason] } : { value: fallback, reason: "ERROR", errorCode: "TYPE_MISMATCH" };
    },
    object(key, fallback, context) {
      const r = run(key, context);
      if (!r) return miss(fallback);
      if (!r.enabled || r.value === null) return { value: fallback, reason: r.enabled ? "DEFAULT" : REASONS[r.reason] };
      try {
        return { value: JSON.parse(r.value), reason: REASONS[r.reason] };
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
    const r = resolver(() => this.client.all(), () => this.client.segments());
    this.resolveBooleanEvaluation = (key, fallback, context) => r.boolean(key, fallback, context);
    this.resolveStringEvaluation = (key, fallback, context) => r.string(key, fallback, context);
    this.resolveNumberEvaluation = (key, fallback, context) => r.number(key, fallback, context);
    this.resolveObjectEvaluation = (key, fallback, context) => r.object(key, fallback, context);
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
    this.segments = {};
    this.at = 0;
    if (!/^ry_flags_[A-Za-z0-9_-]{24,64}$/.test(options.key ?? "")) throw new Error("RailyardServerFlagsProvider needs the environment's flags key (ry_flags_...)");
    const r = resolver(() => this.cached ?? {}, () => this.segments);
    const wrap = (fn) => async (key, fallback, context) => {
      await this.load();
      return fn(key, fallback, context);
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
        this.cached = Object.fromEntries(
          Object.entries(flags).map(([k, f]) => {
            const row = { enabled: f?.enabled === true, value: typeof f?.value === "string" ? f.value : null };
            const t = f?.targeting;
            if (t && typeof t === "object" && Array.isArray(t.rules)) row.targeting = { on: t.on === true, value: typeof t.value === "string" ? t.value : null, rules: t.rules };
            return [k, row];
          }),
        );
        this.segments = body.segments && typeof body.segments === "object" && !Array.isArray(body.segments) ? body.segments : {};
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
