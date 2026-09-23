export type CustomerRole = "owner" | "admin" | "viewer";
export type CustomerStatus = "invited" | "active" | "disabled";
export type KeyEnvironment = "live" | "test";

export interface MarkupPolicy {
  markup_bps: number;
  min_fee_micros: number;
  platform_fee_bps: number;
  funding_mode: "tenant_reserve";
}

export interface CustomerUser {
  id: string;
  membership_id: string;
  tenant_id: string;
  email: string;
  display_name: string;
  role: CustomerRole;
  status: CustomerStatus;
  external_providers: string[];
  created_at: string;
}

export interface CustomerSession {
  user: CustomerUser;
  session_id: string;
  session_token: string;
  expires_at: string;
}

export interface Tenant {
  id: string;
  name: string;
  mint_account_id: string;
}

export interface VendApp {
  id: string;
  tenant_id: string;
  name: string;
  model_allowlist: string[];
  zdr: boolean;
  enabled: boolean;
  markup_policy: MarkupPolicy;
  mint_account_id: string;
}

export interface FundingCheckout {
  provider: string;
  checkout_id: string;
  checkout_url: string;
  deposit_ref: string;
  account_id: string;
  amount_micros: number;
}

export interface CreateApp {
  name: string;
  mint_account_id: string;
  model_allowlist?: string[];
  zdr?: boolean;
  markup_policy: MarkupPolicy;
}

export interface ProvisioningKey {
  id: string;
  tenant_id: string;
  app_id: string | null;
  scope: string;
  status: string;
  prefix: string;
  daily_cap_micros: number | null;
  per_end_user_daily_cap_micros: number | null;
}

export interface IssuedKey {
  key: ProvisioningKey;
  /** Returned once. Persist it in a secret manager, never application logs. */
  secret: string;
}

export interface IssueKey {
  environment?: KeyEnvironment;
  daily_cap_micros: number;
  per_end_user_daily_cap_micros?: number | null;
}

export interface UsageSummary {
  app_id: string;
  utc_day: string;
  requests: number;
  pending_requests: number;
  captured_requests: number;
  released_requests: number;
  pending_charge_micros: number;
  captured_charge_micros: number;
}

export interface SettlementHeaders {
  chargeId: string | null;
  baseCostMicros: number | null;
  priceMicros: number | null;
  model: string | null;
  requestId: string | null;
}

export interface ChatResult<T = unknown> {
  body: T;
  status: number;
  settlement: SettlementHeaders;
}

export interface SSEEvent {
  event: string | null;
  data: string;
  id: string | null;
  retry: number | null;
}

export interface AppBalance {
  account_id: string;
  available_micros: number;
  held_micros: number;
  updated_seq: number;
}

export interface UsageRange {
  from: string;
  to: string;
  days: { utc_day: string; usage: UsageSummary }[];
}

export interface InvitedUser {
  user: CustomerUser;
  setup_token: string;
  setup_expires_at: string;
}

export interface EndUser {
  id: string;
  app_id: string;
  external_ref: string;
  mint_account_id: string | null;
}

export interface VendModel {
  id: string;
  object: "model";
  owned_by: string;
  pricing: {
    input_micros_per_million_tokens: number;
    output_micros_per_million_tokens: number;
    min_fee_micros_per_request: number;
    max_input_tokens: number;
    max_output_tokens: number;
  };
}

export interface VendClientOptions {
  baseUrl: string;
  customerSession?: string;
  provisioningKey?: string;
  fetch?: typeof globalThis.fetch;
}

export class VendApiError extends Error {
  readonly status: number;
  readonly code: string | null;
  readonly body: unknown;
  readonly settlement: SettlementHeaders;

  constructor(status: number, body: unknown, settlement: SettlementHeaders) {
    const error = isRecord(body) && isRecord(body.error) ? body.error : null;
    const message = error && typeof error.message === "string"
      ? error.message
      : `Vend request failed with HTTP ${status}`;
    super(message);
    this.name = "VendApiError";
    this.status = status;
    this.code = error && typeof error.code === "string" ? error.code : null;
    this.body = body;
    this.settlement = settlement;
  }
}

export class VendChatStream implements AsyncIterable<SSEEvent> {
  readonly status: number;
  readonly settlement: SettlementHeaders;
  private readonly response: Response;
  private consumed = false;

  constructor(response: Response) {
    this.response = response;
    this.status = response.status;
    this.settlement = settlementHeaders(response.headers);
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<SSEEvent> {
    if (this.consumed) throw new Error("a Vend stream can only be consumed once");
    this.consumed = true;
    const reader = this.response.body?.getReader();
    if (!reader) throw new Error("Vend returned a streaming response without a body");
    const decoder = new TextDecoder();
    let buffer = "";
    let finished = false;
    try {
      while (true) {
        const chunk = await reader.read();
        if (chunk.done) {
          finished = true;
          buffer += decoder.decode();
          break;
        }
        buffer += decoder.decode(chunk.value, { stream: true });
        const frames = splitFrames(buffer);
        buffer = frames.remainder;
        for (const frame of frames.complete) yield parseEvent(frame);
      }
      if (buffer.trim() !== "") yield parseEvent(buffer);
    } finally {
      if (!finished) await reader.cancel().catch(() => undefined);
      reader.releaseLock();
    }
  }
}

export class VendClient {
  readonly baseUrl: string;
  private customerSession: string | undefined;
  private provisioningKey: string | undefined;
  private readonly fetcher: typeof globalThis.fetch;

  constructor(options: VendClientOptions) {
    this.baseUrl = normalizeBaseUrl(options.baseUrl);
    this.customerSession = options.customerSession;
    this.provisioningKey = options.provisioningKey;
    this.fetcher = options.fetch ?? globalThis.fetch;
    if (!this.fetcher) throw new Error("VendClient requires a Fetch API implementation");
  }

  setCustomerSession(session: string | undefined): void {
    this.customerSession = session;
  }

  setProvisioningKey(key: string | undefined): void {
    this.provisioningKey = key;
  }

  oauthStartUrl(provider: "github" | "google", tenantId: string): string {
    const url = new URL(resolveUrl(`/v1/auth/${provider}/start`, this.baseUrl));
    url.searchParams.set("tenant_id", tenantId);
    return url.toString();
  }

  async setup(setupToken: string, password: string): Promise<CustomerUser> {
    return this.control<CustomerUser>("/v1/auth/setup", "POST", {
      setup_token: setupToken,
      password,
    }, false);
  }

  async login(tenantId: string, email: string, password: string): Promise<CustomerSession> {
    const session = await this.control<CustomerSession>("/v1/auth/login", "POST", {
      tenant_id: tenantId,
      email,
      password,
    }, false);
    this.customerSession = session.session_token;
    return session;
  }

  async logout(): Promise<{ logged_out: true }> {
    const result = await this.control<{ logged_out: true }>("/v1/customer/logout", "POST");
    this.customerSession = undefined;
    return result;
  }

  tenant(): Promise<Tenant> {
    return this.control<Tenant>("/v1/customer/tenant", "GET");
  }

  listApps(): Promise<VendApp[]> {
    return this.control<VendApp[]>("/v1/customer/apps", "GET");
  }

  getApp(appId: string): Promise<VendApp> {
    return this.control<VendApp>(`/v1/customer/apps/${segment(appId)}`, "GET");
  }

  createFundingCheckout(
    appId: string,
    provider: string,
    amountMicros: number,
  ): Promise<FundingCheckout> {
    return this.control<FundingCheckout>(
      `/v1/customer/apps/${segment(appId)}/funding-checkout`,
      "POST",
      { provider, amount_micros: amountMicros },
    );
  }

  createApp(input: CreateApp): Promise<VendApp> {
    return this.control<VendApp>("/v1/customer/apps", "POST", input);
  }

  setAppEnabled(appId: string, enabled: boolean): Promise<VendApp> {
    return this.control<VendApp>(`/v1/customer/apps/${segment(appId)}/enabled`, "PATCH", { enabled });
  }

  listKeys(appId: string): Promise<ProvisioningKey[]> {
    return this.control<ProvisioningKey[]>(`/v1/customer/apps/${segment(appId)}/keys`, "GET");
  }

  issueKey(appId: string, input: IssueKey): Promise<IssuedKey> {
    return this.control<IssuedKey>(`/v1/customer/apps/${segment(appId)}/keys`, "POST", {
      environment: input.environment ?? "live",
      daily_cap_micros: input.daily_cap_micros,
      per_end_user_daily_cap_micros: input.per_end_user_daily_cap_micros ?? null,
    });
  }

  revokeKey(keyId: string): Promise<ProvisioningKey> {
    return this.control<ProvisioningKey>(`/v1/customer/keys/${segment(keyId)}`, "DELETE");
  }

  usage(appId: string, utcDay?: string): Promise<UsageSummary> {
    const path = new URL(resolveUrl(`/v1/customer/apps/${segment(appId)}/usage`, this.baseUrl));
    if (utcDay) path.searchParams.set("utc_day", utcDay);
    return this.control<UsageSummary>(path.toString(), "GET");
  }

  /** Inclusive per-day usage series (max 92 days). */
  usageRange(appId: string, from: string, to: string): Promise<UsageRange> {
    const path = new URL(resolveUrl(`/v1/customer/apps/${segment(appId)}/usage`, this.baseUrl));
    path.searchParams.set("from", from);
    path.searchParams.set("to", to);
    return this.control<UsageRange>(path.toString(), "GET");
  }

  /** Live Mint balance of the app's reserve wallet. */
  balance(appId: string): Promise<AppBalance> {
    return this.control<AppBalance>(`/v1/customer/apps/${segment(appId)}/balance`, "GET");
  }

  provisionEndUser(appId: string, externalRef: string, mintAccountId: string): Promise<EndUser> {
    return this.control<EndUser>(`/v1/customer/apps/${segment(appId)}/end-users`, "POST", {
      external_ref: externalRef,
      mint_account_id: mintAccountId,
    });
  }

  listUsers(): Promise<CustomerUser[]> {
    return this.control<CustomerUser[]>("/v1/customer/users", "GET");
  }

  inviteUser(email: string, displayName: string, role: CustomerRole): Promise<InvitedUser> {
    return this.control<InvitedUser>("/v1/customer/users", "POST", {
      email,
      display_name: displayName,
      role,
    });
  }

  updateUser(userId: string, changes: { role?: CustomerRole; disabled?: boolean }): Promise<CustomerUser> {
    return this.control<CustomerUser>(`/v1/customer/users/${segment(userId)}`, "PATCH", changes);
  }

  revokeUserSessions(userId: string): Promise<unknown> {
    return this.control<unknown>(`/v1/customer/users/${segment(userId)}/sessions`, "DELETE");
  }

  changePassword(currentPassword: string, newPassword: string): Promise<unknown> {
    return this.control<unknown>("/v1/customer/password", "PATCH", {
      current_password: currentPassword,
      new_password: newPassword,
    });
  }

  /** Rotate the current session; the client switches to the new token. */
  async rotateSession(): Promise<CustomerSession> {
    const session = await this.control<CustomerSession>("/v1/customer/session/rotate", "POST");
    this.customerSession = session.session_token;
    return session;
  }

  /** OpenAI-compatible model list for the provisioning key, with effective rates. */
  async listModels(): Promise<VendModel[]> {
    if (!this.provisioningKey) throw new Error("provisioningKey is required to list models");
    const response = await this.fetcher(resolveUrl("/v1/models", this.baseUrl), {
      method: "GET",
      headers: { Accept: "application/json", Authorization: `Bearer ${this.provisioningKey}` },
    });
    const payload: unknown = await response.json().catch(() => null);
    if (!response.ok || !isRecord(payload) || !Array.isArray(payload.data)) {
      throw new VendApiError(response.status, payload, settlementHeaders(response.headers));
    }
    return payload.data as VendModel[];
  }

  async chatCompletions<T = unknown>(
    body: Record<string, unknown>,
    idempotencyKey: string,
    endUser?: string,
  ): Promise<ChatResult<T>> {
    const response = await this.chatRequest({ ...body, stream: false }, idempotencyKey, endUser);
    const payload: unknown = await response.json().catch(() => null);
    const settlement = settlementHeaders(response.headers);
    if (!response.ok) throw new VendApiError(response.status, payload, settlement);
    return { body: payload as T, status: response.status, settlement };
  }

  async chatCompletionsStream(
    body: Record<string, unknown>,
    idempotencyKey: string,
    endUser?: string,
  ): Promise<VendChatStream> {
    const response = await this.chatRequest({ ...body, stream: true }, idempotencyKey, endUser);
    if (!response.ok) {
      const payload: unknown = await response.json().catch(() => null);
      throw new VendApiError(response.status, payload, settlementHeaders(response.headers));
    }
    return new VendChatStream(response);
  }

  private async control<T>(
    path: string,
    method: string,
    body?: unknown,
    authenticated = true,
  ): Promise<T> {
    const headers = new Headers({ Accept: "application/json" });
    if (body !== undefined) headers.set("Content-Type", "application/json");
    if (authenticated) {
      if (!this.customerSession) throw new Error("customerSession is required for this operation");
      headers.set("Authorization", `Bearer ${this.customerSession}`);
    }
    const response = await this.fetcher(resolveUrl(path, this.baseUrl), {
      method,
      headers,
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
      redirect: "manual",
    });
    const payload: unknown = await response.json().catch(() => null);
    if (!response.ok || !isRecord(payload) || payload.success !== true || !("data" in payload)) {
      throw new VendApiError(response.status, payload, settlementHeaders(response.headers));
    }
    return payload.data as T;
  }

  private chatRequest(body: Record<string, unknown>, idempotencyKey: string, endUser?: string): Promise<Response> {
    if (!this.provisioningKey) throw new Error("provisioningKey is required for chat completions");
    if (!idempotencyKey) throw new Error("idempotencyKey is required for chat completions");
    const headers = new Headers({
      Accept: body.stream === true ? "text/event-stream" : "application/json",
      Authorization: `Bearer ${this.provisioningKey}`,
      "Content-Type": "application/json",
      "Idempotency-Key": idempotencyKey,
    });
    if (endUser) headers.set("Vend-End-User", endUser);
    return this.fetcher(resolveUrl("/v1/chat/completions", this.baseUrl), {
      method: "POST",
      headers,
      body: JSON.stringify(body),
      redirect: "manual",
    });
  }
}

function normalizeBaseUrl(value: string): string {
  const url = new URL(value);
  url.search = "";
  url.hash = "";
  return url.toString();
}

function resolveUrl(path: string, baseUrl: string): string {
  try { return new URL(path).toString(); } catch {
    return new URL(path.replace(/^\/+/, ""), `${baseUrl.replace(/\/+$/, "")}/`).toString();
  }
}

function segment(value: string): string {
  return encodeURIComponent(value);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null;
}

function parseIntegerHeader(headers: Headers, name: string): number | null {
  const raw = headers.get(name);
  if (raw === null || !/^-?\d+$/.test(raw)) return null;
  const value = Number(raw);
  return Number.isSafeInteger(value) ? value : null;
}

function settlementHeaders(headers: Headers): SettlementHeaders {
  return {
    chargeId: headers.get("Vend-Charge-Id"),
    baseCostMicros: parseIntegerHeader(headers, "Vend-Base-Cost-Micros"),
    priceMicros: parseIntegerHeader(headers, "Vend-Price-Micros"),
    model: headers.get("Vend-Model"),
    requestId: headers.get("X-Request-Id"),
  };
}

function splitFrames(buffer: string): { complete: string[]; remainder: string } {
  const normalized = buffer.replace(/\r\n/g, "\n");
  const parts = normalized.split("\n\n");
  return { complete: parts.slice(0, -1), remainder: parts.at(-1) ?? "" };
}

function parseEvent(frame: string): SSEEvent {
  let event: string | null = null;
  let id: string | null = null;
  let retry: number | null = null;
  const data: string[] = [];
  for (const line of frame.split("\n")) {
    if (line === "" || line.startsWith(":")) continue;
    const colon = line.indexOf(":");
    const field = colon < 0 ? line : line.slice(0, colon);
    let value = colon < 0 ? "" : line.slice(colon + 1);
    if (value.startsWith(" ")) value = value.slice(1);
    if (field === "data") data.push(value);
    else if (field === "event") event = value;
    else if (field === "id") id = value;
    else if (field === "retry" && /^\d+$/.test(value)) retry = Number(value);
  }
  return { event, data: data.join("\n"), id, retry };
}
