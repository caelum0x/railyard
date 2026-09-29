// Repo over the console's Scribase project REST API (PostgREST), schema
// `railyard`, with the project's service role. Server-only.

import type { Repo } from "./repo.ts";
import type { ApiToken, App, BillingEvent, Deploy, DeviceCode, Entitlement, Org } from "./types.ts";

type Fetch = typeof fetch;

export class PostgrestRepo implements Repo {
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

  async one<T>(path: string): Promise<T | null> {
    const { rows } = await this.req<T>("GET", path);
    return rows[0] ?? null;
  }

  async findOrgByUser(userId: string) {
    const m = await this.one<{ org_id: string }>(`org_members?select=org_id&user_id=eq.${encodeURIComponent(userId)}&limit=1`);
    return m ? this.getOrg(m.org_id) : null;
  }
  async createOrgForUser({ userId, email, name }: { userId: string; email: string; name: string }) {
    const existing = await this.findOrgByUser(userId);
    if (existing) return existing;
    const { rows } = await this.req<Org>("POST", "orgs", { name, owner_user_id: userId }, "return=representation");
    const org = rows[0] ?? (await this.one<Org>(`orgs?owner_user_id=eq.${encodeURIComponent(userId)}`));
    if (!org) throw new Error("could not create the organization");
    await this.req("POST", "org_members", { org_id: org.id, user_id: userId, email, role: "owner" }, "resolution=ignore-duplicates");
    return org;
  }
  async getOrg(orgId: string) {
    return this.one<Org>(`orgs?id=eq.${encodeURIComponent(orgId)}`);
  }
  async getEntitlement(orgId: string) {
    return this.one<Entitlement>(`entitlements?org_id=eq.${encodeURIComponent(orgId)}`);
  }
  async saveEntitlement(ent: Entitlement) {
    const { rows } = await this.req<Entitlement>("POST", "entitlements?on_conflict=org_id", ent, "resolution=merge-duplicates,return=representation");
    return rows[0] ?? ent;
  }
  async recordBillingEvent(ev: BillingEvent) {
    const { status } = await this.req("POST", "billing_events", ev, "return=minimal");
    return status !== 409;
  }
  async releaseBillingEvent(webhookId: string) {
    await this.req("DELETE", `billing_events?webhook_id=eq.${encodeURIComponent(webhookId)}`);
  }
  async findBillingEventByPayment(paymentId: string, kind: string) {
    return this.one<BillingEvent>(`billing_events?payment_id=eq.${encodeURIComponent(paymentId)}&kind=eq.${encodeURIComponent(kind)}&limit=1`);
  }
  async listBillingEvents(orgId: string) {
    return (await this.req<BillingEvent>("GET", `billing_events?org_id=eq.${encodeURIComponent(orgId)}&order=received_at.desc&limit=50`)).rows;
  }
  async listApps(orgId: string) {
    return (await this.req<App>("GET", `apps?org_id=eq.${encodeURIComponent(orgId)}&order=created_at.asc`)).rows;
  }
  async createApp({ orgId, name, siteKey }: { orgId: string; name: string; siteKey: string }) {
    const { status, rows } = await this.req<App>("POST", "apps", { org_id: orgId, name, site_key: siteKey }, "return=representation");
    if (status === 409 || !rows[0]) throw new Error("app_exists");
    return rows[0];
  }
  async getApp(orgId: string, appId: string) {
    return this.one<App>(`apps?org_id=eq.${encodeURIComponent(orgId)}&id=eq.${encodeURIComponent(appId)}`);
  }
  async findAppByName(orgId: string, name: string) {
    return this.one<App>(`apps?org_id=eq.${encodeURIComponent(orgId)}&name=eq.${encodeURIComponent(name)}`);
  }
  async listDeploys(orgId: string, appId?: string) {
    const app = appId ? `&app_id=eq.${encodeURIComponent(appId)}` : "";
    return (await this.req<Deploy>("GET", `deploys?org_id=eq.${encodeURIComponent(orgId)}${app}&order=created_at.desc&limit=100`)).rows;
  }
  async getDeploy(orgId: string, deployId: string) {
    return this.one<Deploy>(`deploys?org_id=eq.${encodeURIComponent(orgId)}&id=eq.${encodeURIComponent(deployId)}`);
  }
  async createDeploy(input: Omit<Deploy, "id" | "created_at">) {
    const { rows } = await this.req<Deploy>("POST", "deploys", input, "return=representation");
    if (!rows[0]) throw new Error("could not record the deploy");
    return rows[0];
  }
  async updateDeploy(orgId: string, deployId: string, patch: Partial<Pick<Deploy, "status" | "duration_ms" | "url" | "version_id">>) {
    const { rows } = await this.req<Deploy>("PATCH", `deploys?org_id=eq.${encodeURIComponent(orgId)}&id=eq.${encodeURIComponent(deployId)}`, patch, "return=representation");
    return rows[0] ?? null;
  }
  async createToken(input: Omit<ApiToken, "id" | "created_at" | "last_used_at" | "revoked_at">) {
    const { rows } = await this.req<ApiToken>("POST", "api_tokens", input, "return=representation");
    if (!rows[0]) throw new Error("could not create the token");
    return rows[0];
  }
  async findTokenByHash(hash: string) {
    return this.one<ApiToken>(`api_tokens?token_hash=eq.${encodeURIComponent(hash)}&revoked_at=is.null`);
  }
  async createDeviceCode(dc: DeviceCode) {
    await this.req("POST", "device_codes", dc, "return=minimal");
  }
  async findDeviceByUserCode(userCode: string) {
    return this.one<DeviceCode>(`device_codes?user_code=eq.${encodeURIComponent(userCode)}`);
  }
  async findDeviceByHash(hash: string) {
    return this.one<DeviceCode>(`device_codes?device_code_hash=eq.${encodeURIComponent(hash)}`);
  }
  async approveDevice(userCode: string, orgId: string, userId: string, at: string) {
    const { rows } = await this.req<DeviceCode>(
      "PATCH",
      `device_codes?user_code=eq.${encodeURIComponent(userCode)}&approved_at=is.null&expires_at=gt.${encodeURIComponent(at)}`,
      { org_id: orgId, user_id: userId, approved_at: at },
      "return=representation",
    );
    return rows.length === 1;
  }
  async consumeDevice(hash: string, at: string) {
    const { rows } = await this.req<DeviceCode>(
      "PATCH",
      `device_codes?device_code_hash=eq.${encodeURIComponent(hash)}&approved_at=not.is.null&consumed_at=is.null`,
      { consumed_at: at },
      "return=representation",
    );
    return rows.length === 1;
  }
}
