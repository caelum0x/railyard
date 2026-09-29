// Data access for the console (repository pattern). Two implementations share
// this interface: `MemoryRepo` (tests and local dev without a database) and
// `PostgrestRepo` (the console's Scribase project, schema `railyard`).

import type { AiCredential, AiKey, AiProvider, AiTrace, ApiToken, App, BillingEvent, ConnectToken, Deploy, DeviceCode, Entitlement, Org, ScribaseBinding } from "./types.ts";

export interface Repo {
  findOrgByUser(userId: string): Promise<Org | null>;
  createOrgForUser(input: { userId: string; email: string; name: string }): Promise<Org>;
  getOrg(orgId: string): Promise<Org | null>;
  getEntitlement(orgId: string): Promise<Entitlement | null>;
  saveEntitlement(ent: Entitlement): Promise<Entitlement>;
  /** Inserts the event; false when its webhook id was already recorded (a replay). */
  recordBillingEvent(ev: BillingEvent): Promise<boolean>;
  releaseBillingEvent(webhookId: string): Promise<void>;
  findBillingEventByPayment(paymentId: string, kind: string): Promise<BillingEvent | null>;
  listBillingEvents(orgId: string): Promise<BillingEvent[]>;
  listApps(orgId: string): Promise<App[]>;
  createApp(input: { orgId: string; name: string; siteKey: string }): Promise<App>;
  getApp(orgId: string, appId: string): Promise<App | null>;
  findAppByName(orgId: string, name: string): Promise<App | null>;
  listDeploys(orgId: string, appId?: string): Promise<Deploy[]>;
  getDeploy(orgId: string, deployId: string): Promise<Deploy | null>;
  createDeploy(input: Omit<Deploy, "id" | "created_at">): Promise<Deploy>;
  updateDeploy(orgId: string, deployId: string, patch: Partial<Pick<Deploy, "status" | "duration_ms" | "url" | "version_id">>): Promise<Deploy | null>;
  createToken(input: Omit<ApiToken, "id" | "created_at" | "last_used_at" | "revoked_at">): Promise<ApiToken>;
  findTokenByHash(hash: string): Promise<ApiToken | null>;
  createDeviceCode(dc: DeviceCode): Promise<void>;
  findDeviceByUserCode(userCode: string): Promise<DeviceCode | null>;
  findDeviceByHash(hash: string): Promise<DeviceCode | null>;
  approveDevice(userCode: string, orgId: string, userId: string, at: string): Promise<boolean>;
  consumeDevice(hash: string, at: string): Promise<boolean>;
  // Scribase bridge (Wave P PY1.0, migration 0002).
  findScribaseBinding(scribaseOrgId: string): Promise<ScribaseBinding | null>;
  /** Binds the org; returns the existing binding when the Scribase org is already bound. */
  bindScribaseOrg(scribaseOrgId: string, orgId: string): Promise<ScribaseBinding>;
  /** Records a bridge message id; false when it was seen before (a replay). */
  recordBridgeMessage(messageId: string): Promise<boolean>;
  // AI gateway (Wave P PY1.4, migration 0003).
  createAiKey(input: Omit<AiKey, "id" | "created_at" | "revoked_at">): Promise<AiKey>;
  findAiKeyByHash(hash: string): Promise<AiKey | null>;
  listAiKeys(orgId: string): Promise<AiKey[]>;
  revokeAiKey(orgId: string, keyId: string, at: string): Promise<void>;
  saveAiCredential(cred: Omit<AiCredential, "updated_at">): Promise<void>;
  getAiCredential(orgId: string, provider: AiProvider): Promise<AiCredential | null>;
  listAiCredentials(orgId: string): Promise<AiCredential[]>;
  /** Adds tokens to the key's UTC day and counts one request (atomic). */
  addAiUsage(keyId: string, day: string, tokens: number): Promise<void>;
  /** Tokens used by the key from `fromDay` (inclusive) on. */
  aiTokensSince(keyId: string, fromDay: string): Promise<number>;
  addAiTrace(trace: AiTrace): Promise<void>;
  listAiTraces(keyIds: string[], limit: number): Promise<AiTrace[]>;
  createConnectToken(token: ConnectToken): Promise<void>;
  /** Marks a live token used; returns it, or null when unknown, used or expired. */
  consumeConnectToken(hash: string, at: string): Promise<ConnectToken | null>;
}

const clone = <T>(v: T): T => structuredClone(v);

export class MemoryRepo implements Repo {
  orgs = new Map<string, Org>();
  members: { org_id: string; user_id: string; email: string }[] = [];
  ents = new Map<string, Entitlement>();
  events: BillingEvent[] = [];
  apps: App[] = [];
  deploys: Deploy[] = [];
  tokens: ApiToken[] = [];
  devices: DeviceCode[] = [];
  bindings: ScribaseBinding[] = [];
  aiKeys: AiKey[] = [];
  aiCredentials: AiCredential[] = [];
  aiUsage: { key_id: string; day: string; tokens: number; requests: number }[] = [];
  aiTraces: AiTrace[] = [];
  connectTokens: ConnectToken[] = [];
  bridgeMessages: string[] = [];
  now: () => Date;

  constructor(now: () => Date = () => new Date()) {
    this.now = now;
  }

  async findOrgByUser(userId: string) {
    const m = this.members.find((x) => x.user_id === userId);
    return m ? clone(this.orgs.get(m.org_id) ?? null) : null;
  }
  async createOrgForUser({ userId, email, name }: { userId: string; email: string; name: string }) {
    const existing = await this.findOrgByUser(userId);
    if (existing) return existing;
    const org: Org = { id: crypto.randomUUID(), name, owner_user_id: userId, created_at: this.now().toISOString() };
    this.orgs.set(org.id, org);
    this.members = [...this.members, { org_id: org.id, user_id: userId, email }];
    return clone(org);
  }
  async getOrg(orgId: string) {
    return clone(this.orgs.get(orgId) ?? null);
  }
  async getEntitlement(orgId: string) {
    return clone(this.ents.get(orgId) ?? null);
  }
  async saveEntitlement(ent: Entitlement) {
    this.ents.set(ent.org_id, clone(ent));
    return clone(ent);
  }
  async recordBillingEvent(ev: BillingEvent) {
    if (this.events.some((e) => e.webhook_id === ev.webhook_id)) return false;
    this.events = [...this.events, { ...clone(ev), received_at: this.now().toISOString() }];
    return true;
  }
  async releaseBillingEvent(webhookId: string) {
    this.events = this.events.filter((e) => e.webhook_id !== webhookId);
  }
  async findBillingEventByPayment(paymentId: string, kind: string) {
    return clone(this.events.find((e) => e.payment_id === paymentId && e.kind === kind) ?? null);
  }
  async listBillingEvents(orgId: string) {
    return clone(this.events.filter((e) => e.org_id === orgId).reverse());
  }
  async listApps(orgId: string) {
    return clone(this.apps.filter((a) => a.org_id === orgId));
  }
  async createApp({ orgId, name, siteKey }: { orgId: string; name: string; siteKey: string }) {
    if (this.apps.some((a) => a.org_id === orgId && a.name === name)) throw new Error("app_exists");
    const app: App = { id: crypto.randomUUID(), org_id: orgId, name, site_key: siteKey, created_at: this.now().toISOString() };
    this.apps = [...this.apps, app];
    return clone(app);
  }
  async getApp(orgId: string, appId: string) {
    return clone(this.apps.find((a) => a.org_id === orgId && a.id === appId) ?? null);
  }
  async findAppByName(orgId: string, name: string) {
    return clone(this.apps.find((a) => a.org_id === orgId && a.name === name) ?? null);
  }
  async listDeploys(orgId: string, appId?: string) {
    return clone(
      this.deploys
        .filter((d) => d.org_id === orgId && (!appId || d.app_id === appId))
        .sort((a, b) => (a.created_at < b.created_at ? 1 : a.created_at > b.created_at ? -1 : 0)),
    );
  }
  async getDeploy(orgId: string, deployId: string) {
    return clone(this.deploys.find((d) => d.org_id === orgId && d.id === deployId) ?? null);
  }
  async createDeploy(input: Omit<Deploy, "id" | "created_at">) {
    const d: Deploy = { ...clone(input), id: crypto.randomUUID(), created_at: this.now().toISOString() };
    this.deploys = [...this.deploys, d];
    return clone(d);
  }
  async updateDeploy(orgId: string, deployId: string, patch: Partial<Pick<Deploy, "status" | "duration_ms" | "url" | "version_id">>) {
    const cur = this.deploys.find((d) => d.org_id === orgId && d.id === deployId);
    if (!cur) return null;
    const next = { ...cur, ...clone(patch) };
    this.deploys = this.deploys.map((d) => (d.id === deployId ? next : d));
    return clone(next);
  }
  async createToken(input: Omit<ApiToken, "id" | "created_at" | "last_used_at" | "revoked_at">) {
    const t: ApiToken = { ...clone(input), id: crypto.randomUUID(), created_at: this.now().toISOString(), last_used_at: null, revoked_at: null };
    this.tokens = [...this.tokens, t];
    return clone(t);
  }
  async findTokenByHash(hash: string) {
    return clone(this.tokens.find((t) => t.token_hash === hash && !t.revoked_at) ?? null);
  }
  async createDeviceCode(dc: DeviceCode) {
    this.devices = [...this.devices, clone(dc)];
  }
  async findDeviceByUserCode(userCode: string) {
    return clone(this.devices.find((d) => d.user_code === userCode) ?? null);
  }
  async findDeviceByHash(hash: string) {
    return clone(this.devices.find((d) => d.device_code_hash === hash) ?? null);
  }
  async approveDevice(userCode: string, orgId: string, userId: string, at: string) {
    const d = this.devices.find((x) => x.user_code === userCode && !x.approved_at && x.expires_at > at);
    if (!d) return false;
    this.devices = this.devices.map((x) => (x === d ? { ...x, org_id: orgId, user_id: userId, approved_at: at } : x));
    return true;
  }
  async consumeDevice(hash: string, at: string) {
    const d = this.devices.find((x) => x.device_code_hash === hash && x.approved_at && !x.consumed_at);
    if (!d) return false;
    this.devices = this.devices.map((x) => (x === d ? { ...x, consumed_at: at } : x));
    return true;
  }
  async findScribaseBinding(scribaseOrgId: string) {
    return clone(this.bindings.find((b) => b.scribase_org_id === scribaseOrgId) ?? null);
  }
  async bindScribaseOrg(scribaseOrgId: string, orgId: string) {
    const existing = this.bindings.find((b) => b.scribase_org_id === scribaseOrgId);
    if (existing) return clone(existing);
    const b: ScribaseBinding = { scribase_org_id: scribaseOrgId, org_id: orgId, created_at: this.now().toISOString() };
    this.bindings = [...this.bindings, b];
    return clone(b);
  }
  async recordBridgeMessage(messageId: string) {
    if (this.bridgeMessages.includes(messageId)) return false;
    this.bridgeMessages = [...this.bridgeMessages, messageId];
    return true;
  }
  async createAiKey(input: Omit<AiKey, "id" | "created_at" | "revoked_at">) {
    const k: AiKey = { ...clone(input), id: crypto.randomUUID(), created_at: this.now().toISOString(), revoked_at: null };
    this.aiKeys = [...this.aiKeys, k];
    return clone(k);
  }
  async findAiKeyByHash(hash: string) {
    return clone(this.aiKeys.find((k) => k.key_hash === hash && !k.revoked_at) ?? null);
  }
  async listAiKeys(orgId: string) {
    return clone(this.aiKeys.filter((k) => k.org_id === orgId));
  }
  async revokeAiKey(orgId: string, keyId: string, at: string) {
    this.aiKeys = this.aiKeys.map((k) => (k.org_id === orgId && k.id === keyId && !k.revoked_at ? { ...k, revoked_at: at } : k));
  }
  async saveAiCredential(cred: Omit<AiCredential, "updated_at">) {
    const row: AiCredential = { ...clone(cred), updated_at: this.now().toISOString() };
    this.aiCredentials = [...this.aiCredentials.filter((c) => !(c.org_id === cred.org_id && c.provider === cred.provider)), row];
  }
  async getAiCredential(orgId: string, provider: AiProvider) {
    return clone(this.aiCredentials.find((c) => c.org_id === orgId && c.provider === provider) ?? null);
  }
  async listAiCredentials(orgId: string) {
    return clone(this.aiCredentials.filter((c) => c.org_id === orgId));
  }
  async addAiUsage(keyId: string, day: string, tokens: number) {
    const cur = this.aiUsage.find((u) => u.key_id === keyId && u.day === day);
    const next = { key_id: keyId, day, tokens: (cur?.tokens ?? 0) + Math.max(0, tokens), requests: (cur?.requests ?? 0) + 1 };
    this.aiUsage = [...this.aiUsage.filter((u) => u !== cur), next];
  }
  async aiTokensSince(keyId: string, fromDay: string) {
    return this.aiUsage.filter((u) => u.key_id === keyId && u.day >= fromDay).reduce((sum, u) => sum + u.tokens, 0);
  }
  async addAiTrace(trace: AiTrace) {
    this.aiTraces = [...this.aiTraces, clone(trace)];
  }
  async listAiTraces(keyIds: string[], limit: number) {
    return clone(this.aiTraces.filter((t) => keyIds.includes(t.key_id)).sort((a, b) => (a.at < b.at ? 1 : -1)).slice(0, limit));
  }
  async createConnectToken(token: ConnectToken) {
    this.connectTokens = [...this.connectTokens, clone(token)];
  }
  async consumeConnectToken(hash: string, at: string) {
    const t = this.connectTokens.find((x) => x.token_hash === hash && !x.used_at && x.expires_at > at);
    if (!t) return null;
    const used = { ...t, used_at: at };
    this.connectTokens = this.connectTokens.map((x) => (x === t ? used : x));
    return clone(used);
  }
}
