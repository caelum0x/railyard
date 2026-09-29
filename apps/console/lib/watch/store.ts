// Storage for Railyard Watch: MemoryWatchStore for tests and local dev,
// PostgrestWatchStore for the console's Scribase project (migration 0006).

import type { Check, CheckStatus } from "./types.ts";

export interface WatchStore {
  createCheck(input: Omit<Check, "id" | "created_at">): Promise<Check>;
  listChecks(orgId: string): Promise<Check[]>;
  getCheck(id: string): Promise<Check | null>;
  findCheckBySlug(slug: string): Promise<Check | null>;
  /** Every check that is not paused (the agent's list). */
  activeChecks(): Promise<Check[]>;
  setPaused(orgId: string, id: string, paused: boolean): Promise<Check | null>;
  getStatus(checkId: string): Promise<CheckStatus | null>;
  saveStatus(status: CheckStatus): Promise<void>;
}

const clone = <T>(v: T): T => structuredClone(v);

export class MemoryWatchStore implements WatchStore {
  checks: Check[] = [];
  statuses = new Map<string, CheckStatus>();
  async createCheck(input: Omit<Check, "id" | "created_at">) {
    if (this.checks.some((c) => c.slug === input.slug)) throw new Error("duplicate slug");
    const row: Check = { ...clone(input), id: crypto.randomUUID(), created_at: new Date().toISOString() };
    this.checks = [...this.checks, row];
    return clone(row);
  }
  async listChecks(orgId: string) {
    return clone(this.checks.filter((c) => c.org_id === orgId));
  }
  async getCheck(id: string) {
    return clone(this.checks.find((c) => c.id === id) ?? null);
  }
  async findCheckBySlug(slug: string) {
    return clone(this.checks.find((c) => c.slug === slug) ?? null);
  }
  async activeChecks() {
    return clone(this.checks.filter((c) => !c.paused));
  }
  async setPaused(orgId: string, id: string, paused: boolean) {
    const found = this.checks.find((c) => c.id === id && c.org_id === orgId);
    if (!found) return null;
    this.checks = this.checks.map((c) => (c.id === id ? { ...c, paused } : c));
    return clone({ ...found, paused });
  }
  async getStatus(checkId: string) {
    return clone(this.statuses.get(checkId) ?? null);
  }
  async saveStatus(status: CheckStatus) {
    this.statuses.set(status.check_id, clone(status));
  }
}

type Fetch = typeof fetch;

export class PostgrestWatchStore implements WatchStore {
  base: string;
  key: string;
  fetchImpl: Fetch;

  constructor(opts: { url: string; serviceRoleKey: string; fetchImpl?: Fetch }) {
    this.base = `${opts.url.replace(/\/$/, "")}/rest/v1`;
    this.key = opts.serviceRoleKey;
    this.fetchImpl = opts.fetchImpl ?? fetch;
  }

  async req<T>(method: string, path: string, body?: unknown, prefer?: string): Promise<T[]> {
    const headers: Record<string, string> = {
      apikey: this.key,
      authorization: `Bearer ${this.key}`,
      "accept-profile": "railyard",
      "content-profile": "railyard",
      "content-type": "application/json",
    };
    if (prefer) headers.prefer = prefer;
    const res = await this.fetchImpl(`${this.base}/${path}`, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
    if (!res.ok) throw new Error(`database ${method} ${path.split("?")[0]} failed with ${res.status}`);
    const text = await res.text();
    return text ? (JSON.parse(text) as T[]) : [];
  }

  async createCheck(input: Omit<Check, "id" | "created_at">) {
    const rows = await this.req<Check>("POST", "watch_checks", input, "return=representation");
    if (!rows[0]) throw new Error("could not create the check");
    return rows[0];
  }
  async listChecks(orgId: string) {
    return this.req<Check>("GET", `watch_checks?org_id=eq.${encodeURIComponent(orgId)}&order=created_at.asc`);
  }
  async getCheck(id: string) {
    return (await this.req<Check>("GET", `watch_checks?id=eq.${encodeURIComponent(id)}&limit=1`))[0] ?? null;
  }
  async findCheckBySlug(slug: string) {
    return (await this.req<Check>("GET", `watch_checks?slug=eq.${encodeURIComponent(slug)}&limit=1`))[0] ?? null;
  }
  async activeChecks() {
    return this.req<Check>("GET", "watch_checks?paused=is.false&order=created_at.asc&limit=10000");
  }
  async setPaused(orgId: string, id: string, paused: boolean) {
    const rows = await this.req<Check>("PATCH", `watch_checks?id=eq.${encodeURIComponent(id)}&org_id=eq.${encodeURIComponent(orgId)}`, { paused }, "return=representation");
    return rows[0] ?? null;
  }
  async getStatus(checkId: string) {
    return (await this.req<CheckStatus>("GET", `watch_status?check_id=eq.${encodeURIComponent(checkId)}&limit=1`))[0] ?? null;
  }
  async saveStatus(status: CheckStatus) {
    await this.req("POST", "watch_status?on_conflict=check_id", status, "resolution=merge-duplicates,return=minimal");
  }
}
