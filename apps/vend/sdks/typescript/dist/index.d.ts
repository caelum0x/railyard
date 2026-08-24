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
export interface VendClientOptions {
    baseUrl: string;
    customerSession?: string;
    provisioningKey?: string;
    fetch?: typeof globalThis.fetch;
}
export declare class VendApiError extends Error {
    readonly status: number;
    readonly code: string | null;
    readonly body: unknown;
    readonly settlement: SettlementHeaders;
    constructor(status: number, body: unknown, settlement: SettlementHeaders);
}
export declare class VendChatStream implements AsyncIterable<SSEEvent> {
    readonly status: number;
    readonly settlement: SettlementHeaders;
    private readonly response;
    private consumed;
    constructor(response: Response);
    [Symbol.asyncIterator](): AsyncGenerator<SSEEvent>;
}
export declare class VendClient {
    readonly baseUrl: string;
    private customerSession;
    private provisioningKey;
    private readonly fetcher;
    constructor(options: VendClientOptions);
    setCustomerSession(session: string | undefined): void;
    setProvisioningKey(key: string | undefined): void;
    oauthStartUrl(provider: "github" | "google", tenantId: string): string;
    setup(setupToken: string, password: string): Promise<CustomerUser>;
    login(tenantId: string, email: string, password: string): Promise<CustomerSession>;
    logout(): Promise<{
        logged_out: true;
    }>;
    tenant(): Promise<Tenant>;
    listApps(): Promise<VendApp[]>;
    getApp(appId: string): Promise<VendApp>;
    createApp(input: CreateApp): Promise<VendApp>;
    setAppEnabled(appId: string, enabled: boolean): Promise<VendApp>;
    listKeys(appId: string): Promise<ProvisioningKey[]>;
    issueKey(appId: string, input: IssueKey): Promise<IssuedKey>;
    revokeKey(keyId: string): Promise<ProvisioningKey>;
    usage(appId: string, utcDay?: string): Promise<UsageSummary>;
    chatCompletions<T = unknown>(body: Record<string, unknown>, idempotencyKey: string, endUser?: string): Promise<ChatResult<T>>;
    chatCompletionsStream(body: Record<string, unknown>, idempotencyKey: string, endUser?: string): Promise<VendChatStream>;
    private control;
    private chatRequest;
}
