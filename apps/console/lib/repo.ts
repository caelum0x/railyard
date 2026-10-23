// Data access for the console (repository pattern). Two implementations share
// this interface: `MemoryRepo` (tests and local dev without a database) and
// `PostgrestRepo` (the console's Scribase project, schema `railyard`).

import type { AiCredential, AiKey, AiProvider, AiTrace, ApiToken, App, BillingEvent, ConnectToken, Deploy, DeviceCode, Entitlement, OidcCode, Org, ScribaseBinding } from "./types.ts";

export interface Repo {
  findOrgByUser(userId: string): Promise<Org | null>;
  createOrgForUser(input: { userId: string; email: string; name: string; signupSource?: string | null }): Promise<Org>;
  getOrg(orgId: string): Promise<Org | null>;
  getEntitlement(orgId: string): Promise<Entitlement | null>;
  saveEntitlement(ent: Entitlement): Promise<Entitlement>;
  /**
   * Saves an add-on entitlement only if no newer Scribase state is stored
   * (`addon_as_of` greater than `asOf`); `null` when it was stale. `asOf`
   * null saves unconditionally (the legacy bridge sender).
   */
  saveAddonEntitlement(ent: Entitlement, asOf: number | null): Promise<Entitlement | null>;
  /** Inserts the event; false when its webhook id was already recorded (a replay). */
  recordBillingEvent(ev: BillingEvent): Promise<boolean>;
  releaseBillingEvent(webhookId: string): Promise<void>;
  findBillingEventByPayment(paymentId: string, kind: string): Promise<BillingEvent | null>;
  listBillingEvents(orgId: string): Promise<BillingEvent[]>;
  listApps(orgId: string): Promise<App[]>;
  createApp(input: { orgId: string; name: string; siteKey: string }): Promise<App>;
  getApp(orgId: string, appId: string): Promise<App | null>;
  findAppByName(orgId: string, name: string): Promise<App | null>;
  /** Records the Mint billing account minted for this app (backlog item 11); idempotent, last write wins. */
  setAppMintAccount(orgId: string, appId: string, mintAccountId: string): Promise<App | null>;
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
  // Rithub self-serve provisioning (backlog item 4, migration 0014).
  /** Records the Gitea user/org/repo provisioned for this org; idempotent, last write wins. */
  setOrgGitea(orgId: string, input: { username: string; orgSlug: string; repoUrl: string }): Promise<Org | null>;
  // OIDC provider (backlog item 5, migration 0013).
  createOidcCode(code: Omit<OidcCode, "created_at" | "consumed_at">): Promise<void>;
  /** Marks a live code consumed once; returns it, or null when unknown, already consumed or expired. */
  consumeOidcCode(hash: string, at: string): Promise<OidcCode | null>;
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
  oidcCodes: OidcCode[] = [];
  bridgeMessages: string[] = [];
  now: () => Date;

  constructor(now: () => Date = () => new Date()) {
    this.now = now;
  }

  async findOrgByUser(userId: string) {
    const m = this.members.find((x) => x.user_id === userId);
    return m ? clone(this.orgs.get(m.org_id) ?? null) : null;
  }
  async createOrgForUser({ userId, email, name, signupSource }: { userId: string; email: string; name: string; signupSource?: string | null }) {
    const existing = await this.findOrgByUser(userId);
    if (existing) return existing;
    const org: Org = {
      id: crypto.randomUUID(),
      name,
      owner_user_id: userId,
      created_at: this.now().toISOString(),
      signup_source: signupSource ?? null,
      gitea_username: null,
      gitea_org_slug: null,
      gitea_repo_url: null,
    };
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
  async saveAddonEntitlement(ent: Entitlement, asOf: number | null) {
    const stored = this.ents.get(ent.org_id)?.addon_as_of ?? null;
    if (asOf !== null && stored !== null && stored > asOf) return null;
    const next = { ...clone(ent), addon_as_of: asOf ?? stored };
    this.ents.set(ent.org_id, next);
    return clone(next);
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
    const app: App = { id: crypto.randomUUID(), org_id: orgId, name, site_key: siteKey, created_at: this.now().toISOString(), mint_account_id: null };
    this.apps = [...this.apps, app];
    return clone(app);
  }
  async getApp(orgId: string, appId: string) {
    return clone(this.apps.find((a) => a.org_id === orgId && a.id === appId) ?? null);
  }
  async findAppByName(orgId: string, name: string) {
    return clone(this.apps.find((a) => a.org_id === orgId && a.name === name) ?? null);
  }
  async setAppMintAccount(orgId: string, appId: string, mintAccountId: string) {
    const app = this.apps.find((a) => a.org_id === orgId && a.id === appId);
    if (!app) return null;
    app.mint_account_id = mintAccountId;
    return clone(app);
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
  async setOrgGitea(orgId: string, input: { username: string; orgSlug: string; repoUrl: string }) {
    const org = this.orgs.get(orgId);
    if (!org) return null;
    const next: Org = { ...org, gitea_username: input.username, gitea_org_slug: input.orgSlug, gitea_repo_url: input.repoUrl };
    this.orgs.set(orgId, next);
    return clone(next);
  }
  async createOidcCode(code: Omit<OidcCode, "created_at" | "consumed_at">) {
    this.oidcCodes = [...this.oidcCodes, { ...clone(code), created_at: this.now().toISOString(), consumed_at: null }];
  }
  async consumeOidcCode(hash: string, at: string) {
    const c = this.oidcCodes.find((x) => x.code_hash === hash && !x.consumed_at && x.expires_at > at);
    if (!c) return null;
    const used = { ...c, consumed_at: at };
    this.oidcCodes = this.oidcCodes.map((x) => (x === c ? used : x));
    return clone(used);
  }
}
