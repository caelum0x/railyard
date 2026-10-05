// Reads an environment's flags through the flags slot's public client API
// only (GET /api/v1/flags/client?key=ry_flags_...), never its tables.
// Answers are kept 15 s, the same as the slot's own edge cache.

import type { Rule, SegmentMap } from "./targeting.mjs";

export interface SlotFlag {
  enabled: boolean;
  value: string | null;
  /** A targeted flag's switch, base value and rules (QY1.3), for per-person evaluation here. */
  targeting?: { on: boolean; value: string | null; rules: Rule[] };
}

/** One environment's flags and the segments their rules use. */
export interface SlotEnv {
  flags: Record<string, SlotFlag>;
  segments: SegmentMap;
}

export interface FlagsReader {
  read(clientKey: string): Promise<Record<string, SlotFlag>>;
  /** Flags with the segments their rules use (QY1.3). */
  readEnv?(clientKey: string): Promise<SlotEnv>;
}

function slotFlag(f: { enabled?: unknown; value?: unknown; targeting?: unknown } | null | undefined): SlotFlag {
  const out: SlotFlag = { enabled: f?.enabled === true, value: typeof f?.value === "string" ? f.value : null };
  const t = f?.targeting as { on?: unknown; value?: unknown; rules?: unknown } | undefined;
  if (t && typeof t === "object" && Array.isArray(t.rules)) out.targeting = { on: t.on === true, value: typeof t.value === "string" ? t.value : null, rules: t.rules as Rule[] };
  return out;
}

export class HttpFlagsReader implements FlagsReader {
  base: string;
  fetchImpl: typeof fetch;
  now: () => number;
  ttlMs: number;
  cache = new Map<string, { at: number; env: SlotEnv }>();

  constructor(opts: { url: string; fetchImpl?: typeof fetch; now?: () => number; ttlMs?: number }) {
    this.base = opts.url.replace(/\/$/, "");
    this.fetchImpl = opts.fetchImpl ?? fetch;
    this.now = opts.now ?? Date.now;
    this.ttlMs = opts.ttlMs ?? 15_000;
  }

  async read(clientKey: string): Promise<Record<string, SlotFlag>> {
    return (await this.readEnv(clientKey)).flags;
  }

  async readEnv(clientKey: string): Promise<SlotEnv> {
    const hit = this.cache.get(clientKey);
    if (hit && this.now() - hit.at < this.ttlMs) return hit.env;
    const res = await this.fetchImpl(`${this.base}/api/v1/flags/client?key=${encodeURIComponent(clientKey)}`, { headers: { accept: "application/json" } });
    if (!res.ok) throw new Error(`flags client read failed with ${res.status}`);
    const body = (await res.json()) as { flags?: Record<string, { enabled?: unknown; value?: unknown; targeting?: unknown }>; segments?: unknown };
    const flags: Record<string, SlotFlag> = {};
    for (const [k, f] of Object.entries(body.flags ?? {})) flags[k] = slotFlag(f);
    const segments: SegmentMap = {};
    if (body.segments && typeof body.segments === "object" && !Array.isArray(body.segments)) {
      for (const [k, seg] of Object.entries(body.segments as Record<string, { conditions?: unknown }>)) if (Array.isArray(seg?.conditions)) segments[k] = { conditions: seg.conditions as SegmentMap[string]["conditions"] };
    }
    const env = { flags, segments };
    if (this.cache.size > 5000) this.cache.clear();
    this.cache.set(clientKey, { at: this.now(), env });
    return env;
  }
}

export class MemoryFlagsReader implements FlagsReader {
  envs: Record<string, Record<string, SlotFlag>>;
  segments: Record<string, SegmentMap>;
  constructor(envs: Record<string, Record<string, SlotFlag>>, segments: Record<string, SegmentMap> = {}) {
    this.envs = envs;
    this.segments = segments;
  }
  async read(clientKey: string) {
    const f = this.envs[clientKey];
    if (!f) throw new Error("flags client read failed with 401");
    return f;
  }
  async readEnv(clientKey: string) {
    return { flags: await this.read(clientKey), segments: this.segments[clientKey] ?? {} };
  }
}

/** No flags slot configured: every project has no flags. */
export const NO_FLAGS: FlagsReader = { read: async () => ({}) };
