// Storage of flag environments (and, for the table backend, flag values).
// MemoryFlagsStore serves tests and local dev; PostgrestFlagsStore talks to
// the console's Scribase project (schema railyard, migration 0005).

import type { Flag, FlagEnv, FlagInput } from "./types.ts";

export interface FlagsStore {
  findEnv(orgId: string, app: string, env: string): Promise<FlagEnv | null>;
  findEnvByClientKey(clientKey: string): Promise<FlagEnv | null>;
  listEnvs(orgId: string): Promise<FlagEnv[]>;
  /** Inserts; returns the existing row when (org, app, env) is already there. */
  createEnv(env: Omit<FlagEnv, "id" | "created_at" | "first_read_at">): Promise<FlagEnv>;
  /** Sets first_read_at once; true when this call set it. */
  markFirstRead(envId: string, at: string): Promise<boolean>;
  // Table backend values.
  listFlags(envId: string): Promise<Flag[]>;
  upsertFlag(envId: string, input: FlagInput, actor: string, at: string): Promise<Flag>;
}

const clone = <T>(v: T): T => structuredClone(v);

export class MemoryFlagsStore implements FlagsStore {
  envs: FlagEnv[] = [];
  values = new Map<string, Map<string, Flag>>();
  async findEnv(orgId: string, app: string, env: string) {
    return clone(this.envs.find((e) => e.org_id === orgId && e.app === app && e.env === env) ?? null);
  }
  async findEnvByClientKey(clientKey: string) {
    return clone(this.envs.find((e) => e.client_key === clientKey) ?? null);
  }
  async listEnvs(orgId: string) {
    return clone(this.envs.filter((e) => e.org_id === orgId));
  }
  async createEnv(input: Omit<FlagEnv, "id" | "created_at" | "first_read_at">) {
    const existing = await this.findEnv(input.org_id, input.app, input.env);
    if (existing) return existing;
    const row: FlagEnv = { ...clone(input), id: crypto.randomUUID(), created_at: new Date().toISOString(), first_read_at: null };
    this.envs = [...this.envs, row];
    return clone(row);
  }
  async markFirstRead(envId: string, at: string) {
    const row = this.envs.find((e) => e.id === envId);
    if (!row || row.first_read_at) return false;
    this.envs = this.envs.map((e) => (e.id === envId ? { ...e, first_read_at: at } : e));
    return true;
  }
  async listFlags(envId: string) {
    return clone([...(this.values.get(envId)?.values() ?? [])].sort((a, b) => a.key.localeCompare(b.key)));
  }
  async upsertFlag(envId: string, input: FlagInput, _actor: string, at: string) {
    const flags = new Map(this.values.get(envId) ?? []);
    const prev = flags.get(input.key);
    const row: Flag = {
      key: input.key,
      enabled: input.enabled,
      value: input.value === undefined ? (prev?.value ?? null) : input.value,
      description: input.description === undefined ? (prev?.description ?? null) : input.description,
      updated_at: at,
    };
    flags.set(input.key, row);
    this.values.set(envId, flags);
    return clone(row);
  }
}

type Fetch = typeof fetch;

export class PostgrestFlagsStore implements FlagsStore {
  base: string;
  key: string;
  fetchImpl: Fetch;

  constructor(opts: { url: string; serviceRoleKey: string; fetchImpl?: Fetch }) {
    this.base = `${opts.url.replace(/\/$/, "")}/rest/v1`;
    this.key = opts.serviceRoleKey;
    this.fetchImpl = opts.fetchImpl ?? fetch;
  }

  async req<T>(method: string, path: string, body?: unknown, prefer?: string): Promise<{ status: number; rows: T[] }> {
    const headers: Record<string, string> = {
      apikey: this.key,
      authorization: `Bearer ${this.key}`,
      "accept-profile": "railyard",
      "content-profile": "railyard",
      "content-type": "application/json",
    };
    if (prefer) headers.prefer = prefer;
    const res = await this.fetchImpl(`${this.base}/${path}`, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
    if (res.status === 409) return { status: 409, rows: [] };
    if (!res.ok) throw new Error(`database ${method} ${path.split("?")[0]} failed with ${res.status}`);
    const text = await res.text();
    return { status: res.status, rows: text ? (JSON.parse(text) as T[]) : [] };
  }

  async findEnv(orgId: string, app: string, env: string) {
    const q = `flag_envs?org_id=eq.${encodeURIComponent(orgId)}&app=eq.${encodeURIComponent(app)}&env=eq.${encodeURIComponent(env)}&limit=1`;
    return (await this.req<FlagEnv>("GET", q)).rows[0] ?? null;
  }
  async findEnvByClientKey(clientKey: string) {
    return (await this.req<FlagEnv>("GET", `flag_envs?client_key=eq.${encodeURIComponent(clientKey)}&limit=1`)).rows[0] ?? null;
  }
  async listEnvs(orgId: string) {
    return (await this.req<FlagEnv>("GET", `flag_envs?org_id=eq.${encodeURIComponent(orgId)}&order=app.asc,env.asc`)).rows;
  }
  async createEnv(input: Omit<FlagEnv, "id" | "created_at" | "first_read_at">) {
    const { rows } = await this.req<FlagEnv>("POST", "flag_envs?on_conflict=org_id,app,env", input, "resolution=ignore-duplicates,return=representation");
    const row = rows[0] ?? (await this.findEnv(input.org_id, input.app, input.env));
    if (!row) throw new Error("could not create the flag environment");
    return row;
  }
  async markFirstRead(envId: string, at: string) {
    const { rows } = await this.req<FlagEnv>("PATCH", `flag_envs?id=eq.${encodeURIComponent(envId)}&first_read_at=is.null`, { first_read_at: at }, "return=representation");
    return rows.length > 0;
  }
  async listFlags(envId: string) {
    return (await this.req<Flag>("GET", `flags?env_id=eq.${encodeURIComponent(envId)}&select=key,enabled,value,description,updated_at&order=key.asc`)).rows;
  }
  async upsertFlag(envId: string, input: FlagInput, actor: string, at: string) {
    const row: Record<string, unknown> = { env_id: envId, key: input.key, enabled: input.enabled, updated_at: at, updated_by: actor };
    if (input.value !== undefined) row.value = input.value;
    if (input.description !== undefined) row.description = input.description;
    const { rows } = await this.req<Flag>("POST", "flags?on_conflict=env_id,key&select=key,enabled,value,description,updated_at", row, "resolution=merge-duplicates,return=representation");
    if (!rows[0]) throw new Error("could not save the flag");
    return rows[0];
  }
}
