// Railyard Flags client (Wave P PY2.1). Reads one environment's flags with its
// public flags key and keeps them fresh: it polls every `pollMs` (30 s by
// default; the read endpoint caches 15 s, so a flip reaches the app within
// 60 s with no redeploy). The last good answer is kept in `storage` when one is
// given (localStorage, AsyncStorage or anything with getItem/setItem), so an
// app that starts offline still has its last flags. Plain fetch, no
// dependencies: works in browsers, Next.js (server and client) and Expo.
//
// Targeting (Wave Q QY1.3): a targeted flag carries its rules and the
// environment's segments; evaluate(key, ctx) answers for any person locally,
// with the same code and hash the server uses.

import { evaluateAnswer } from "./targeting.mjs";

export const DEFAULT_URL = "https://railyard.ereboslab.com/api/v1/flags/client";
export const DEFAULT_POLL_MS = 30_000;
export const MIN_POLL_MS = 5_000;
const KEY = /^ry_flags_[A-Za-z0-9_-]{24,64}$/;

/** @param {unknown} v */
function asFlags(v) {
  if (!v || typeof v !== "object" || Array.isArray(v)) return null;
  const flags = /** @type {Record<string, unknown>} */ (v).flags;
  if (!flags || typeof flags !== "object" || Array.isArray(flags)) return null;
  /** @type {Record<string, {enabled: boolean, value: string | null}>} */
  const out = {};
  for (const [k, f] of Object.entries(flags)) {
    if (!f || typeof f !== "object") continue;
    const row = /** @type {Record<string, unknown>} */ (f);
    out[k] = { enabled: row.enabled === true, value: typeof row.value === "string" ? row.value : null };
    const t = /** @type {Record<string, unknown> | undefined} */ (row.targeting);
    if (t && typeof t === "object" && Array.isArray(t.rules)) out[k].targeting = { on: t.on === true, value: typeof t.value === "string" ? t.value : null, rules: t.rules };
  }
  return out;
}

/** @param {unknown} v */
function asSegments(v) {
  const segs = v && typeof v === "object" ? /** @type {Record<string, unknown>} */ (v).segments : null;
  if (!segs || typeof segs !== "object" || Array.isArray(segs)) return {};
  /** @type {Record<string, {conditions: unknown[]}>} */
  const out = {};
  for (const [k, seg] of Object.entries(segs)) if (seg && typeof seg === "object" && Array.isArray(/** @type {any} */ (seg).conditions)) out[k] = { conditions: /** @type {any} */ (seg).conditions };
  return out;
}

const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

/**
 * @param {{ key: string, url?: string, pollMs?: number, fetch?: typeof fetch,
 *   storage?: { getItem(k: string): unknown, setItem(k: string, v: string): unknown } | null,
 *   setTimeout?: typeof setTimeout, clearTimeout?: typeof clearTimeout }} options
 */
export function createFlagsClient(options) {
  if (!options || typeof options.key !== "string" || !KEY.test(options.key)) {
    throw new Error("createFlagsClient needs the environment's flags key (ry_flags_...)");
  }
  const url = `${(options.url ?? DEFAULT_URL).replace(/\/$/, "")}?key=${encodeURIComponent(options.key)}`;
  const pollMs = Math.max(MIN_POLL_MS, options.pollMs ?? DEFAULT_POLL_MS);
  const doFetch = options.fetch ?? globalThis.fetch.bind(globalThis);
  const storage = options.storage ?? null;
  const later = options.setTimeout ?? globalThis.setTimeout.bind(globalThis);
  const cancel = options.clearTimeout ?? globalThis.clearTimeout.bind(globalThis);
  const storageKey = `railyard-flags:${options.key.slice(-12)}`;
  /** @type {Record<string, {enabled: boolean, value: string | null}>} */
  let flags = {};
  /** @type {Record<string, {conditions: unknown[]}>} */
  let segments = {};
  let source = "default";
  let timer = null;
  let closed = false;
  let failures = 0;
  const listeners = new Set();

  const set = (next, from) => {
    const changed = !same(next, flags);
    flags = next;
    source = from;
    if (changed) for (const fn of listeners) {
      try {
        fn(flags);
      } catch {
        // A listener's error never stops the others or the polling.
      }
    }
    return changed;
  };

  async function restore() {
    if (!storage) return false;
    try {
      const raw = await storage.getItem(storageKey);
      const parsed = typeof raw === "string" ? JSON.parse(raw) : null;
      const saved = asFlags(parsed);
      if (saved && source === "default") {
        segments = asSegments(parsed);
        set(saved, "cache");
        return true;
      }
    } catch {
      // A broken cache entry is ignored.
    }
    return false;
  }

  /** Reads the flags once. Resolves true when the answer was new. */
  async function refresh() {
    const res = await doFetch(url, { headers: { accept: "application/json" } });
    if (!res.ok) throw new Error(`flags read answered ${res.status}`);
    const body = await res.json();
    const next = asFlags(body);
    if (!next) throw new Error("flags read answered an unexpected shape");
    failures = 0;
    const nextSegments = asSegments(body);
    const segmentsChanged = !same(nextSegments, segments);
    segments = nextSegments;
    const changed = set(next, "live") || segmentsChanged;
    if (storage) {
      try {
        await storage.setItem(storageKey, JSON.stringify({ flags: next, segments }));
      } catch {
        // Storage full or unavailable: the flags still work for this session.
      }
    }
    return changed;
  }

  function schedule() {
    if (closed) return;
    // Back off on errors (up to 4x), keep the last good flags meanwhile.
    const wait = pollMs * Math.min(4, 2 ** failures);
    timer = later(async () => {
      try {
        await refresh();
      } catch {
        failures += 1;
      }
      schedule();
    }, wait);
  }

  let started = null;
  return {
    /** Loads the cached flags, then the live ones; starts polling. Never rejects. */
    ready() {
      if (!started) {
        started = (async () => {
          await restore();
          try {
            await refresh();
          } catch {
            failures += 1;
          }
          schedule();
          return source !== "default";
        })();
      }
      return started;
    },
    refresh,
    isEnabled(key, fallback = false) {
      return Object.hasOwn(flags, key) ? flags[key].enabled : fallback;
    },
    getValue(key, fallback = null) {
      return Object.hasOwn(flags, key) && flags[key].value !== null ? flags[key].value : fallback;
    },
    has(key) {
      return Object.hasOwn(flags, key);
    },
    all() {
      return { ...flags };
    },
    /**
     * The flag for one person: `{enabled, value, reason, rule}`. Plain flags
     * answer as `isEnabled`/`getValue` do; targeted flags run their rules.
     * @param {string} key
     * @param {{ id?: string | null, properties?: Record<string, unknown>, cohorts?: string[] }} [ctx]
     */
    evaluate(key, ctx = {}) {
      return Object.hasOwn(flags, key) ? evaluateAnswer(key, flags[key], ctx, segments) : null;
    },
    segments() {
      return { ...segments };
    },
    /** "default" before any answer, "cache" from storage, "live" from the server. */
    source() {
      return source;
    },
    subscribe(fn) {
      listeners.add(fn);
      return () => listeners.delete(fn);
    },
    close() {
      closed = true;
      if (timer !== null) cancel(timer);
      listeners.clear();
    },
  };
}
