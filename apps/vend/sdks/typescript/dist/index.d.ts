export type CustomerRole = "owner" | "admin" | "viewer";
export type CustomerStatus = "invited" | "active" | "disabled";
export type KeyEnvironment = "live" | "test";
export interface MarkupPolicy {
    markup_bps: number;
    min_fee_micros: number;
    platform_fee_bps: number;
    funding_mode: "tenant_reserve" | "end_user_reserve";
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
    days: {
        utc_day: string;
        usage: UsageSummary;
    }[];
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
export interface Wallet {
    external_ref: string;
    end_user_id: string;
    mint_account_id: string;
    available_micros: number;
    held_micros: number;
    spent_today_micros: number;
    funding_mode: MarkupPolicy["funding_mode"];
    created: boolean;
}
export type TopUpProvider = "creem" | "polar" | "paypal" | "stripe" | "iyzico" | "usdc";
export interface WalletListItem {
    end_user_id: string;
    external_ref: string;
    mint_account_id: string | null;
    spent_today_micros: number;
    created_at_ms: number;
    available_micros?: number;
    held_micros?: number;
    balance_error?: string;
}
export interface ModelMarkup {
    markup_bps: number;
    min_fee_micros?: number;
}
export interface ModelPrice {
    model: string;
    provider_input_micros_per_million_tokens: number;
    provider_output_micros_per_million_tokens: number;
    override: ModelMarkup | null;
    effective_markup_bps: number;
    effective_min_fee_micros: number;
    input_micros_per_million_tokens: number;
    output_micros_per_million_tokens: number;
}
export interface PriceSheet {
    app_id: string;
    default: MarkupPolicy;
    models: ModelPrice[];
}
export interface EarningsReport {
    from: string;
    to: string;
    summary: {
        captured_requests: number;
        charge_micros: number;
        provider_cost_micros: number;
        platform_fee_micros: number;
        tenant_net_micros: number;
    };
    days: Array<{
        utc_day: string;
        captured_requests: number;
        charge_micros: number;
        provider_cost_micros: number;
        platform_fee_micros: number;
        tenant_net_micros: number;
    }>;
    payouts: {
        totals: {
            complete_count: number;
            pending_count: number;
            tenant_paid_micros: number;
            tenant_pending_micros: number;
            platform_paid_micros: number;
        };
        recent: Array<Record<string, unknown>>;
    };
}
export interface SignupInput {
    workspace_name: string;
    email: string;
    display_name: string;
    password: string;
    app_name?: string;
}
export interface SignupResult {
    tenant_id: string;
    app_id: string;
    app_mint_account_id: string;
    tenant_mint_account_id: string;
    key_id: string;
    provisioning_key: string;
    session_token: string;
    expires_at: string;
    funding_mode: MarkupPolicy["funding_mode"];
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
    createFundingCheckout(appId: string, provider: string, amountMicros: number): Promise<FundingCheckout>;
    createApp(input: CreateApp): Promise<VendApp>;
    setAppEnabled(appId: string, enabled: boolean): Promise<VendApp>;
    listKeys(appId: string): Promise<ProvisioningKey[]>;
    issueKey(appId: string, input: IssueKey): Promise<IssuedKey>;
    revokeKey(keyId: string): Promise<ProvisioningKey>;
    usage(appId: string, utcDay?: string): Promise<UsageSummary>;
    /** Inclusive per-day usage series (max 92 days). */
    usageRange(appId: string, from: string, to: string): Promise<UsageRange>;
    /** Live Mint balance of the app's reserve wallet. */
    balance(appId: string): Promise<AppBalance>;
    provisionEndUser(appId: string, externalRef: string, mintAccountId: string): Promise<EndUser>;
    listUsers(): Promise<CustomerUser[]>;
    inviteUser(email: string, displayName: string, role: CustomerRole): Promise<InvitedUser>;
    updateUser(userId: string, changes: {
        role?: CustomerRole;
        disabled?: boolean;
    }): Promise<CustomerUser>;
    revokeUserSessions(userId: string): Promise<unknown>;
    changePassword(currentPassword: string, newPassword: string): Promise<unknown>;
    /** Rotate the current session; the client switches to the new token. */
    /** Self-serve workspace signup (when the operator enabled it). Signs in. */
    signup(input: SignupInput): Promise<SignupResult>;
    /** Create (idempotently) the wallet of one of your users. */
    createWallet(externalRef: string): Promise<Wallet>;
    /** Live wallet balance and today's spend. */
    getWallet(externalRef: string): Promise<Wallet>;
    /** Hosted checkout that deposits into the user's wallet. Whole cents only. */
    createWalletTopUp(externalRef: string, provider: TopUpProvider, amountMicros: number): Promise<FundingCheckout>;
    listWallets(appId: string, limit?: number, offset?: number): Promise<{
        funding_mode: string;
        wallets: WalletListItem[];
    }>;
    getModelPricing(appId: string): Promise<PriceSheet>;
    setModelMarkup(appId: string, model: string, markup: ModelMarkup): Promise<PriceSheet>;
    clearModelMarkup(appId: string, model: string): Promise<PriceSheet>;
    earnings(from?: string, to?: string): Promise<EarningsReport>;
    rotateSession(): Promise<CustomerSession>;
    /** OpenAI-compatible model list for the provisioning key, with effective rates. */
    listModels(): Promise<VendModel[]>;
    chatCompletions<T = unknown>(body: Record<string, unknown>, idempotencyKey: string, endUser?: string): Promise<ChatResult<T>>;
    chatCompletionsStream(body: Record<string, unknown>, idempotencyKey: string, endUser?: string): Promise<VendChatStream>;
    private control;
    /** `{success,data}` call authenticated with the provisioning key. */
    private keyed;
    private chatRequest;
}
