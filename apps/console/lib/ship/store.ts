// Storage for ship on push (PY3.1): MemoryShipStore for tests and local dev,
// PostgrestShipStore for the console's Scribase project (migration 0007).

import type { CloudflareConnection, GithubInstallation, LinkSession, ShipLink } from "./types.ts";

export interface ShipStore {
  saveInstallation(inst: GithubInstallation): Promise<void>;
  getInstallation(installationId: number): Promise<GithubInstallation | null>;
  deleteInstallation(installationId: number): Promise<void>;
  /** Creates or replaces the link for the repository (one app per repository). */
  saveLink(link: Omit<ShipLink, "id" | "created_at">): Promise<ShipLink>;
  findLinkByRepoId(repoId: number): Promise<ShipLink | null>;
  getLink(orgId: string, id: string): Promise<ShipLink | null>;
  listLinks(orgId: string): Promise<ShipLink[]>;
  deleteLinksByRepoIds(installationId: number, repoIds: number[]): Promise<number>;
  deleteLinksByInstallation(installationId: number): Promise<number>;
  saveCloudflare(conn: CloudflareConnection): Promise<void>;
  getCloudflare(orgId: string): Promise<CloudflareConnection | null>;
  createSession(s: LinkSession): Promise<void>;
  getSession(idHash: string): Promise<LinkSession | null>;
  updateSession(idHash: string, patch: Partial<Pick<LinkSession, "installation_id" | "state" | "message" | "link_id" | "repo_id">>): Promise<void>;
}

const clone = <T>(v: T): T => structuredClone(v);

export class MemoryShipStore implements ShipStore {
  installations = new Map<number, GithubInstallation>();
  links: ShipLink[] = [];
  cloudflare = new Map<string, CloudflareConnection>();
  sessions = new Map<string, LinkSession>();

  async saveInstallation(inst: GithubInstallation) {
    this.installations.set(inst.installation_id, clone(inst));
  }
  async getInstallation(id: number) {
    return clone(this.installations.get(id) ?? null);
  }
  async deleteInstallation(id: number) {
    this.installations.delete(id);
  }
  async saveLink(link: Omit<ShipLink, "id" | "created_at">) {
    const existing = this.links.find((l) => l.repo_id === link.repo_id);
    const row: ShipLink = { ...clone(link), id: existing?.id ?? crypto.randomUUID(), created_at: existing?.created_at ?? new Date().toISOString() };
    this.links = [...this.links.filter((l) => l.repo_id !== link.repo_id), row];
    return clone(row);
  }
  async findLinkByRepoId(repoId: number) {
    return clone(this.links.find((l) => l.repo_id === repoId) ?? null);
  }
  async getLink(orgId: string, id: string) {
    return clone(this.links.find((l) => l.id === id && l.org_id === orgId) ?? null);
  }
  async listLinks(orgId: string) {
    return clone(this.links.filter((l) => l.org_id === orgId));
  }
  async deleteLinksByRepoIds(installationId: number, repoIds: number[]) {
    const before = this.links.length;
    this.links = this.links.filter((l) => !(l.installation_id === installationId && repoIds.includes(l.repo_id)));
    return before - this.links.length;
  }
  async deleteLinksByInstallation(installationId: number) {
    const before = this.links.length;
    this.links = this.links.filter((l) => l.installation_id !== installationId);
    return before - this.links.length;
  }
  async saveCloudflare(conn: CloudflareConnection) {
    this.cloudflare.set(conn.org_id, clone(conn));
  }
  async getCloudflare(orgId: string) {
    return clone(this.cloudflare.get(orgId) ?? null);
  }
  async createSession(s: LinkSession) {
    this.sessions.set(s.id_hash, clone(s));
  }
  async getSession(idHash: string) {
    return clone(this.sessions.get(idHash) ?? null);
  }
  async updateSession(idHash: string, patch: Partial<Pick<LinkSession, "installation_id" | "state" | "message" | "link_id" | "repo_id">>) {
    const s = this.sessions.get(idHash);
    if (s) this.sessions.set(idHash, { ...s, ...clone(patch) });
  }
}

type Fetch = typeof fetch;

export class PostgrestShipStore implements ShipStore {
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

  async saveInstallation(inst: GithubInstallation) {
    await this.req("POST", "github_installations?on_conflict=installation_id", inst, "resolution=merge-duplicates,return=minimal");
  }
  async getInstallation(id: number) {
    return (await this.req<GithubInstallation>("GET", `github_installations?installation_id=eq.${id}&limit=1`))[0] ?? null;
  }
  async deleteInstallation(id: number) {
    await this.req("DELETE", `github_installations?installation_id=eq.${id}`);
  }
  async saveLink(link: Omit<ShipLink, "id" | "created_at">) {
    const rows = await this.req<ShipLink>("POST", "ship_links?on_conflict=repo_id", link, "resolution=merge-duplicates,return=representation");
    if (!rows[0]) throw new Error("could not save the link");
    return rows[0];
  }
  async findLinkByRepoId(repoId: number) {
    return (await this.req<ShipLink>("GET", `ship_links?repo_id=eq.${repoId}&limit=1`))[0] ?? null;
  }
  async getLink(orgId: string, id: string) {
    return (await this.req<ShipLink>("GET", `ship_links?id=eq.${encodeURIComponent(id)}&org_id=eq.${encodeURIComponent(orgId)}&limit=1`))[0] ?? null;
  }
  async listLinks(orgId: string) {
    return this.req<ShipLink>("GET", `ship_links?org_id=eq.${encodeURIComponent(orgId)}&order=created_at.asc`);
  }
  async deleteLinksByRepoIds(installationId: number, repoIds: number[]) {
    if (!repoIds.length) return 0;
    const ids = repoIds.filter((n) => Number.isInteger(n)).join(",");
    return (await this.req("DELETE", `ship_links?installation_id=eq.${installationId}&repo_id=in.(${ids})`, undefined, "return=representation")).length;
  }
  async deleteLinksByInstallation(installationId: number) {
    return (await this.req("DELETE", `ship_links?installation_id=eq.${installationId}`, undefined, "return=representation")).length;
  }
  async saveCloudflare(conn: CloudflareConnection) {
    await this.req("POST", "cloudflare_connections?on_conflict=org_id", conn, "resolution=merge-duplicates,return=minimal");
  }
  async getCloudflare(orgId: string) {
    return (await this.req<CloudflareConnection>("GET", `cloudflare_connections?org_id=eq.${encodeURIComponent(orgId)}&limit=1`))[0] ?? null;
  }
  async createSession(s: LinkSession) {
    await this.req("POST", "ship_link_sessions", s, "return=minimal");
  }
  async getSession(idHash: string) {
    return (await this.req<LinkSession>("GET", `ship_link_sessions?id_hash=eq.${encodeURIComponent(idHash)}&limit=1`))[0] ?? null;
  }
  async updateSession(idHash: string, patch: Partial<Pick<LinkSession, "installation_id" | "state" | "message" | "link_id" | "repo_id">>) {
    await this.req("PATCH", `ship_link_sessions?id_hash=eq.${encodeURIComponent(idHash)}`, patch, "return=minimal");
  }
}
