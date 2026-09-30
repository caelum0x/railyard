// Site directory: token (the app's public site key) -> project, org, limits.
// Read only; the console stays the writer of railyard.apps and entitlements.
//   PostgrestSiteDirectory  the console project's REST API (schema railyard)
//   MemorySiteDirectory     tests and local runs
//   CachedSiteDirectory     60 s cache in front of either

import { retentionDaysFor } from "../plans.ts";

export interface Site {
  /** railyard.apps.id */
  project_id: string;
  org_id: string;
  /** railyard.apps.site_key: the public token the SDK sends. */
  token: string;
  name: string;
  plan: string;
  /** Stored events per UTC month for the whole org (console `access().eventsLimit`). */
  events_limit: number;
  /** A paused org stores nothing. */
  paused: boolean;
  retention_days: number;
  /** The Scribase org bound to this Railyard org, for the usage meter. */
  scribase_org_id: string | null;
}

export interface SiteDirectory {
  byToken(token: string): Promise<Site | null>;
  byProject(projectId: string): Promise<Site | null>;
  byOrg(orgId: string): Promise<Site[]>;
  byScribaseOrg(scribaseOrgId: string): Promise<Site[]>;
  /** Every project, for nightly jobs. */
  all(): Promise<Site[]>;
}

export const TOKEN = /^[A-Za-z0-9_-]{8,128}$/;

export class MemorySiteDirectory implements SiteDirectory {
  sites: Site[];
  constructor(sites: Site[]) {
    this.sites = sites;
  }
  async byToken(token: string) {
    return this.sites.find((s) => s.token === token) ?? null;
  }
  async byProject(id: string) {
    return this.sites.find((s) => s.project_id === id) ?? null;
  }
  async byOrg(orgId: string) {
    return this.sites.filter((s) => s.org_id === orgId);
  }
  async byScribaseOrg(id: string) {
    return this.sites.filter((s) => s.scribase_org_id === id);
  }
  async all() {
    return [...this.sites];
  }
}

interface AppRow {
  id: string;
  org_id: string;
  name: string;
  site_key: string;
}

interface EntitlementRow {
  org_id: string;
  plan: string;
  status: string;
  events_limit: number;
  paused: boolean;
  trial_ends_at: string | null;
}

/** The console's `access()` rule, read only: what an org may store now. */
export function limitsFrom(ent: EntitlementRow | null, now: Date): { events_limit: number; paused: boolean; plan: string } {
  if (!ent) return { events_limit: 0, paused: false, plan: "none" };
  if (ent.plan === "addon") {
    const on = ent.status === "active" && !ent.paused;
    return { events_limit: ent.events_limit, paused: !on, plan: "addon" };
  }
  if (ent.paused) return { events_limit: ent.events_limit, paused: true, plan: ent.plan };
  if (ent.plan === "railyard") return { events_limit: ent.events_limit, paused: false, plan: "railyard" };
  if (ent.plan === "trial" && ent.trial_ends_at && Date.parse(ent.trial_ends_at) > now.getTime()) {
    return { events_limit: ent.events_limit, paused: false, plan: "trial" };
  }
  return { events_limit: 0, paused: false, plan: ent.plan };
}

type Fetch = typeof fetch;

export class PostgrestSiteDirectory implements SiteDirectory {
  base: string;
  key: string;
  fetchImpl: Fetch;
  now: () => Date;

  constructor(opts: { url: string; serviceKey: string; fetchImpl?: Fetch; now?: () => Date }) {
    this.base = `${opts.url.replace(/\/$/, "")}/rest/v1`;
    this.key = opts.serviceKey;
    this.fetchImpl = opts.fetchImpl ?? fetch;
    this.now = opts.now ?? (() => new Date());
  }

  async get<T>(path: string): Promise<T[]> {
    const res = await this.fetchImpl(`${this.base}/${path}`, {
      headers: { apikey: this.key, authorization: `Bearer ${this.key}`, "accept-profile": "railyard" },
    });
    if (!res.ok) throw new Error(`site directory read ${path.split("?")[0]} failed with ${res.status}`);
    return (await res.json()) as T[];
  }

  async build(apps: AppRow[]): Promise<Site[]> {
    if (apps.length === 0) return [];
    const orgs = [...new Set(apps.map((a) => a.org_id))];
    const inList = orgs.map(encodeURIComponent).join(",");
    const [ents, binds] = await Promise.all([
      this.get<EntitlementRow>(`entitlements?select=org_id,plan,status,events_limit,paused,trial_ends_at&org_id=in.(${inList})`),
      this.get<{ org_id: string; scribase_org_id: string }>(`scribase_bindings?select=org_id,scribase_org_id&org_id=in.(${inList})`),
    ]);
    const now = this.now();
    return apps.map((a) => {
      const lim = limitsFrom(ents.find((e) => e.org_id === a.org_id) ?? null, now);
      return {
        project_id: a.id,
        org_id: a.org_id,
        token: a.site_key,
        name: a.name,
        plan: lim.plan,
        events_limit: lim.events_limit,
        paused: lim.paused,
        retention_days: retentionDaysFor(lim.plan, null),
        scribase_org_id: binds.find((b) => b.org_id === a.org_id)?.scribase_org_id ?? null,
      };
    });
  }

  async byToken(token: string) {
    if (!TOKEN.test(token)) return null;
    return (await this.build(await this.get<AppRow>(`apps?select=id,org_id,name,site_key&site_key=eq.${encodeURIComponent(token)}&limit=1`)))[0] ?? null;
  }
  async byProject(id: string) {
    return (await this.build(await this.get<AppRow>(`apps?select=id,org_id,name,site_key&id=eq.${encodeURIComponent(id)}&limit=1`)))[0] ?? null;
  }
  async byOrg(orgId: string) {
    return this.build(await this.get<AppRow>(`apps?select=id,org_id,name,site_key&org_id=eq.${encodeURIComponent(orgId)}&limit=1000`));
  }
  async byScribaseOrg(id: string) {
    const b = await this.get<{ org_id: string }>(`scribase_bindings?select=org_id&scribase_org_id=eq.${encodeURIComponent(id)}&limit=1`);
    return b[0] ? this.byOrg(b[0].org_id) : [];
  }
  async all() {
    return this.build(await this.get<AppRow>("apps?select=id,org_id,name,site_key&limit=10000"));
  }
}

/** A 60 s cache in front of a directory; misses are cached too, so a bad token cannot hammer the source. */
export class CachedSiteDirectory implements SiteDirectory {
  inner: SiteDirectory;
  ttlMs: number;
  now: () => number;
  cache = new Map<string, { at: number; value: unknown }>();

  constructor(inner: SiteDirectory, opts: { ttlMs?: number; now?: () => number } = {}) {
    this.inner = inner;
    this.ttlMs = opts.ttlMs ?? 60_000;
    this.now = opts.now ?? Date.now;
  }

  async memo<T>(key: string, load: () => Promise<T>): Promise<T> {
    const hit = this.cache.get(key);
    const t = this.now();
    if (hit && t - hit.at < this.ttlMs) return hit.value as T;
    const value = await load();
    if (this.cache.size > 10_000) this.cache.clear();
    this.cache.set(key, { at: t, value });
    return value;
  }

  byToken(token: string) {
    return this.memo(`t:${token}`, () => this.inner.byToken(token));
  }
  byProject(id: string) {
    return this.memo(`p:${id}`, () => this.inner.byProject(id));
  }
  byOrg(orgId: string) {
    return this.memo(`o:${orgId}`, () => this.inner.byOrg(orgId));
  }
  byScribaseOrg(id: string) {
    return this.memo(`s:${id}`, () => this.inner.byScribaseOrg(id));
  }
  all() {
    return this.memo("all", () => this.inner.all());
  }
}
