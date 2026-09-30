// Reads an environment's flags through the flags slot's public client API
// only (GET /api/v1/flags/client?key=ry_flags_...), never its tables.
// Answers are kept 15 s, the same as the slot's own edge cache.

export interface SlotFlag {
  enabled: boolean;
  value: string | null;
}

export interface FlagsReader {
  read(clientKey: string): Promise<Record<string, SlotFlag>>;
}

export class HttpFlagsReader implements FlagsReader {
  base: string;
  fetchImpl: typeof fetch;
  now: () => number;
  ttlMs: number;
  cache = new Map<string, { at: number; flags: Record<string, SlotFlag> }>();

  constructor(opts: { url: string; fetchImpl?: typeof fetch; now?: () => number; ttlMs?: number }) {
    this.base = opts.url.replace(/\/$/, "");
    this.fetchImpl = opts.fetchImpl ?? fetch;
    this.now = opts.now ?? Date.now;
    this.ttlMs = opts.ttlMs ?? 15_000;
  }

  async read(clientKey: string): Promise<Record<string, SlotFlag>> {
    const hit = this.cache.get(clientKey);
    if (hit && this.now() - hit.at < this.ttlMs) return hit.flags;
    const res = await this.fetchImpl(`${this.base}/api/v1/flags/client?key=${encodeURIComponent(clientKey)}`, { headers: { accept: "application/json" } });
    if (!res.ok) throw new Error(`flags client read failed with ${res.status}`);
    const body = (await res.json()) as { flags?: Record<string, { enabled?: unknown; value?: unknown }> };
    const flags: Record<string, SlotFlag> = {};
    for (const [k, f] of Object.entries(body.flags ?? {})) {
      flags[k] = { enabled: f?.enabled === true, value: typeof f?.value === "string" ? f.value : null };
    }
    if (this.cache.size > 5000) this.cache.clear();
    this.cache.set(clientKey, { at: this.now(), flags });
    return flags;
  }
}

export class MemoryFlagsReader implements FlagsReader {
  envs: Record<string, Record<string, SlotFlag>>;
  constructor(envs: Record<string, Record<string, SlotFlag>>) {
    this.envs = envs;
  }
  async read(clientKey: string) {
    const f = this.envs[clientKey];
    if (!f) throw new Error("flags client read failed with 401");
    return f;
  }
}

/** No flags slot configured: every project has no flags. */
export const NO_FLAGS: FlagsReader = { read: async () => ({}) };
