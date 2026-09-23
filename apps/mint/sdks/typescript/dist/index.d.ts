export declare const AITP_VERSION = "0.1";
export declare const HEADER_VERSION = "AITP-Version";
export declare const HEADER_CHALLENGE = "AITP-Challenge";
export declare const HEADER_AUTHORIZATION = "AITP-Authorization";
export declare const HEADER_RECEIPT = "AITP-Receipt";
export type AitpScheme = "aitp-hold" | "aitp-transfer";
export interface PaymentChallenge {
    payee: string;
    amount_micros: number;
    currency: "AIT";
    resource: string;
    nonce: string;
    expiry: number;
    scheme: AitpScheme;
}
export interface PaymentAuthorization {
    payer: string;
    payee: string;
    amount_micros: number;
    nonce: string;
    resource: string;
    expiry: number;
    scheme: AitpScheme;
    idempotency_key: string;
    pubkey_id: string;
    op_hash: string;
    sig: string;
}
export interface Receipt {
    seq: number;
    op: string;
    op_hash: string;
    amount_micros: number;
    balance_after_micros: number;
    prev_hash: string;
    hash: string;
    sig: string;
}
export interface SignResult {
    pubkey_id: string;
    /** Lowercase hex BLAKE3 digest of the canonical intent bytes. */
    op_hash: string;
    /** Ed25519 signature of op_hash, base64url without padding. */
    sig: string;
}
export interface WalletSigner {
    signAuthorization(canonicalIntent: Uint8Array): SignResult | Promise<SignResult>;
}
export declare class AuthorizationBuilder {
    readonly payer: string;
    private readonly signer;
    constructor(payer: string, signer: WalletSigner);
    authorize(challenge: PaymentChallenge): Promise<PaymentAuthorization>;
}
export interface HttpRequest {
    method: string;
    url: string;
    headers?: HeadersInit;
    body?: BodyInit | null;
}
export declare class PaidHttpClient {
    private readonly authorizer;
    private readonly fetcher;
    constructor(authorizer: AuthorizationBuilder, fetcher?: typeof globalThis.fetch);
    /** Send once, then sign and retry exactly once for a valid AITP 402. */
    send(request: HttpRequest): Promise<Response>;
}
export declare function encodeAuthorization(value: PaymentAuthorization): string;
export declare function decodeAuthorization(header: string): PaymentAuthorization;
export declare function decodeChallenge(header: string): PaymentChallenge;
export declare function decodeReceipt(header: string): Receipt;
export interface MintClientOptions {
    baseUrl: string;
    apiKey: string;
    fetch?: typeof globalThis.fetch;
}
/** Direct account transfer for trusted backend integrations. */
export interface TransferRequest {
    from: string;
    to: string;
    amount_micros: number;
    idem_key: string;
    ref?: string;
}
export interface AddressLookup {
    account_id: string;
}
export interface CreateAddressResponse {
    address: string;
}
export interface Balance {
    account_id: string;
    available_micros: number;
    held_micros: number;
    updated_seq: number;
}
export interface OpenStreamRequest {
    from: string;
    to: string;
    rate_micros: number;
    cap_micros: number;
    unit: "Second" | "Unit";
    idem_key: string;
    ref?: string;
}
export interface TickStreamRequest {
    units: number;
    seq: number;
    idem_key: string;
    ref?: string;
}
export interface CloseStreamRequest extends TickStreamRequest {
}
export interface ReserveProof {
    supply_micros: number;
    backing_micros: number;
    cushion: number;
    ok: boolean;
}
export interface MintRequest {
    to: string;
    amount_micros: number;
    deposit_ref: string;
    idem_key: string;
}
export interface RedeemRequest {
    from: string;
    amount_micros: number;
    payout_ref: string;
    kyc_tier: "none" | "basic" | "verified";
    payout_dest: {
        kind: "stripe";
        account: string;
    } | {
        kind: "crypto";
        chain: string;
        address: string;
    } | {
        kind: "paypal";
        email: string;
    };
    idem_key: string;
}
export type AccountStatus = "active" | "frozen" | "closed";
export interface AccountSummary {
    id: string;
    account_id: string;
    kind: string;
    owner_ref: string | null;
    status: AccountStatus;
    created_ms: number | null;
    available_micros: number;
    held_micros: number;
    updated_seq: number;
}
export interface Page<T> {
    items: T[];
    next_cursor?: string | null;
}
export type PayoutStatus = "pending" | "paid" | "failed" | "cancelled";
export interface PayoutRecord {
    payout_ref: string;
    provider: string;
    account_id: string;
    amount_micros: number;
    dest: Record<string, unknown>;
    status: PayoutStatus;
    provider_ref: string | null;
    attempts: number;
    last_error: string | null;
    burn_seq: number;
    created_ms: number;
    updated_ms: number;
}
export type ApiScope = "read" | "accounts" | "spend" | "funding" | "payouts" | "admin";
export interface ApiKey {
    id: string;
    name: string;
    prefix: string;
    scopes: ApiScope[];
    accounts: string[] | null;
    created_ms: number;
    revoked_ms: number | null;
    active: boolean;
}
export interface CreatedApiKey extends ApiKey {
    /** The plaintext secret. Returned ONCE; store it in your secret manager. */
    secret: string;
}
export interface ImportReportRequest {
    provider: string;
    content: string;
    label?: string;
    format?: "csv" | "json";
    amount_unit?: "major" | "cents" | "micros";
    reference_column?: string;
}
export type ReversalStatus = "clawed_back" | "shortfall" | "written_off";
export interface DepositReversal {
    deposit_ref: string;
    provider: string;
    account_id: string;
    amount_micros: number;
    clawed_micros: number;
    outstanding_micros: number;
    reason: "refund" | "dispute";
    status: ReversalStatus;
    attempts: number;
    note: string | null;
    created_ms: number;
    updated_ms: number;
    burned_now_micros?: number;
}
export interface VerificationKey {
    key_id: string;
    algorithm: string;
    public_key_hex: string;
    status: string;
}
export interface AitpSettlement {
    scheme: "transfer" | "hold";
    receipt: Record<string, unknown>;
    hold_id?: string;
    /** Hold scheme only: when Mint auto-releases the hold if never captured (unix ms). */
    hold_expires_at_ms?: number | null;
    signed_receipt: Receipt;
}
export type HoldStatus = "Open" | "Captured" | "Released";
export interface HoldPosition {
    id: string;
    account_id: string;
    amount_micros: number;
    captured_micros: number;
    status: HoldStatus;
    ref: string | null;
    /** Auto-release deadline (unix ms); `null` when the hold never expires. */
    expires_at_ms: number | null;
}
export interface StreamPosition {
    id: string;
    from_id: string;
    to_id: string;
    rate_micros: number;
    unit: "Second" | "Unit";
    cap_micros: number;
    settled_micros: number;
    last_seq: number;
    status: "Open" | "Closed";
    ref: string | null;
    direction: "outgoing" | "incoming";
}
export interface AllowancePosition {
    id: string;
    granter: string;
    to_key: string;
    limit_micros: number;
    spent_micros: number;
    window: "PerCall" | {
        Rolling: {
            window_ms: number;
        };
    };
    window_start_ms: number;
    scope: Record<string, unknown> | null;
    revoked: boolean;
}
export interface PayerKey {
    payer: string;
    pubkey_id: string;
    public_key_hex: string;
    label: string | null;
    created_ms: number;
    revoked_ms: number | null;
    active: boolean;
}
/** The hold-settlement response of `aitpCapture` / `aitpRelease`. */
export interface AitpHoldSettlement extends Record<string, unknown> {
    hold_id: string;
    status: string;
    attestation?: Record<string, unknown>;
    signed_receipt: Receipt;
}
export interface TransferReceipt {
    first_seq: number;
    from_balance: Record<string, unknown>;
    to_balance: Record<string, unknown>;
    idempotent_replay: boolean;
    receipt: SignedTransferReceipt;
    attestation: TransferAttestation;
}
export interface TransferReceiptBody {
    account_id: string;
    account_seq: number;
    journal_seq: number;
    op: string;
    op_hash: number[];
    amount_micros: number;
    from: string | null;
    to: string | null;
    balance_after_available_micros: number;
    balance_after_held_micros: number;
    ts: number;
    prev_hash: string;
    signer: string;
}
export interface SignedTransferReceipt {
    body: TransferReceiptBody;
    hash: string;
    sig: string;
}
export interface TransferAttestation {
    format: "mint-transfer-attestation-v1";
    payload_base64url: string;
    digest_sha256: string;
    signer_key_id: string;
    signature_hex: string;
}
export declare class MintApiError extends Error {
    readonly status: number;
    readonly code: string;
    readonly body: unknown;
    constructor(status: number, code: string, body: unknown, message: string);
}
export declare class MintClient {
    readonly baseUrl: string;
    private readonly apiKey;
    private readonly fetcher;
    constructor(options: MintClientOptions);
    createAccount(kind: string, ownerRef?: string): Promise<Record<string, unknown>>;
    account(accountId: string): Promise<Record<string, unknown>>;
    balance(accountId: string): Promise<Record<string, unknown>>;
    createAddress(accountId: string, purpose?: string): Promise<CreateAddressResponse>;
    lookupAddress(address: string): Promise<AddressLookup>;
    createCheckout(provider: string, accountId: string, amountMicros: number): Promise<Record<string, unknown>>;
    /**
     * Move integer microdollars exactly once for one logical operation.
     * Keep this account-id API and the Mint service key behind your backend.
     */
    transfer(input: TransferRequest): Promise<TransferReceipt>;
    /** Resolve a public receive address, then bind the signed proof to its account. */
    transferToAddress(input: Omit<TransferRequest, "to"> & {
        address: string;
    }): Promise<TransferReceipt>;
    createHold(input: Record<string, unknown>): Promise<Record<string, unknown>>;
    hold(holdId: string): Promise<Record<string, unknown>>;
    captureHold(holdId: string, input: Record<string, unknown>): Promise<Record<string, unknown>>;
    releaseHold(holdId: string, input: Record<string, unknown>): Promise<Record<string, unknown>>;
    grantAllowance(input: Record<string, unknown>): Promise<Record<string, unknown>>;
    allowance(allowanceId: string): Promise<Record<string, unknown>>;
    spendAllowance(allowanceId: string, input: Record<string, unknown>): Promise<Record<string, unknown>>;
    revokeAllowance(allowanceId: string, idemKey: string): Promise<{
        revoked: boolean;
    }>;
    openStream(input: OpenStreamRequest): Promise<Record<string, unknown>>;
    stream(streamId: string): Promise<Record<string, unknown>>;
    tickStream(streamId: string, input: TickStreamRequest): Promise<Record<string, unknown>>;
    closeStream(streamId: string, input: CloseStreamRequest): Promise<Record<string, unknown>>;
    accountReceipts(accountId: string): Promise<SignedTransferReceipt[]>;
    receiptAudit(): Promise<{
        merkle_root: string;
        epoch: number;
    }>;
    reserve(): Promise<ReserveProof>;
    mint(input: MintRequest): Promise<Record<string, unknown>>;
    redeem(input: RedeemRequest): Promise<Record<string, unknown>>;
    /** Operator listing of accounts with live balances (keyset paginated). */
    listAccounts(params?: {
        after?: string;
        limit?: number;
    }): Promise<Page<AccountSummary>>;
    /** Period statement: opening balance, entries, totals, closing balance. */
    statement(accountId: string, params?: {
        from_ms?: number;
        to_ms?: number;
    }): Promise<Record<string, unknown>>;
    kycStatus(accountId: string): Promise<Record<string, unknown>>;
    startKycSession(accountId: string): Promise<{
        provider: string;
        url: string;
    } & Record<string, unknown>>;
    /** Block outgoing movement from an account (operator key). */
    freezeAccount(accountId: string): Promise<AccountSummary>;
    unfreezeAccount(accountId: string): Promise<AccountSummary>;
    /** Permanently close a zero-balance account (operator key). */
    closeAccount(accountId: string): Promise<AccountSummary>;
    listPayouts(params?: {
        status?: PayoutStatus;
        account_id?: string;
        limit?: number;
        cursor?: string;
    }): Promise<Page<PayoutRecord>>;
    /** One payout; a pending payout is refreshed from the provider first. */
    payout(payoutRef: string): Promise<PayoutRecord>;
    retryPayout(payoutRef: string): Promise<PayoutRecord>;
    /** Abandon a failed payout and re-credit the burned amount (operator key). */
    cancelPayout(payoutRef: string, note?: string): Promise<PayoutRecord>;
    /** Record an out-of-band settlement of a payout (operator key). */
    markPayoutPaid(payoutRef: string, providerRef?: string): Promise<PayoutRecord>;
    reconciliation(): Promise<Record<string, unknown>>;
    /** Import a provider settlement report and get its match results. */
    importProviderReport(input: ImportReportRequest): Promise<Record<string, unknown>>;
    providerReports(): Promise<{
        items: Record<string, unknown>[];
    }>;
    providerReport(reportId: string): Promise<Record<string, unknown>>;
    deleteProviderReport(reportId: string): Promise<{
        deleted: string;
    }>;
    /** Record a refund / chargeback and claw it back (operator key). */
    recordReversal(input: {
        deposit_ref: string;
        amount_micros?: number;
        reason?: "refund" | "dispute";
        note?: string;
    }): Promise<DepositReversal>;
    reversals(params?: {
        status?: ReversalStatus;
        limit?: number;
    }): Promise<{
        items: DepositReversal[];
    }>;
    retryReversal(depositRef: string): Promise<DepositReversal>;
    writeOffReversal(depositRef: string, note?: string): Promise<DepositReversal>;
    /** DB-backed API keys (operator key). */
    apiKeys(): Promise<{
        items: ApiKey[];
        env_keys: Record<string, unknown>[];
    }>;
    createApiKey(input: {
        name: string;
        scopes: ApiScope[];
        accounts?: string[];
    }): Promise<CreatedApiKey>;
    revokeApiKey(keyId: string): Promise<ApiKey>;
    /** An account's holds with their auto-release deadlines (newest first). */
    accountHolds(accountId: string, params?: {
        status?: "open" | "captured" | "released";
        limit?: number;
    }): Promise<{
        account_id: string;
        items: HoldPosition[];
        open_held_micros: number;
    }>;
    /** Streams the account pays or receives (newest first). */
    accountStreams(accountId: string, params?: {
        status?: "open" | "closed";
        limit?: number;
    }): Promise<{
        account_id: string;
        items: StreamPosition[];
    }>;
    /** Allowances the account granted (`status: "all"` includes revoked ones). */
    accountAllowances(accountId: string, params?: {
        status?: "active" | "all";
        limit?: number;
    }): Promise<{
        account_id: string;
        items: AllowancePosition[];
    }>;
    /** AITP-402 payer verifying keys (operator key). */
    payerKeys(params?: {
        payer?: string;
    }): Promise<{
        items: PayerKey[];
        active_count: number;
    }>;
    /** Register an agent's Ed25519 public key so its AITP authorizations verify. */
    registerPayerKey(input: {
        payer: string;
        pubkey_id: string;
        public_key_hex: string;
        label?: string;
    }): Promise<PayerKey>;
    /** Revoke a payer key; authorizations signed with it are refused immediately. */
    revokePayerKey(payer: string, pubkeyId: string): Promise<PayerKey>;
    /** Capture (part of) an AITP-402 hold settlement to the payee (merchant success path). */
    aitpCapture(input: {
        hold_id: string;
        amount_micros: number;
        to: string;
        idem_key: string;
    }): Promise<AitpHoldSettlement>;
    /** Release an AITP-402 hold settlement back to the payer (merchant failure path). */
    aitpRelease(input: {
        hold_id: string;
        idem_key: string;
    }): Promise<AitpHoldSettlement>;
    private holdSettlement;
    verificationKeys(): Promise<{
        keys: VerificationKey[];
    }>;
    /** Issue the single-use 402 challenge that the payer signs locally. */
    issueAitpChallenge(input: {
        payee: string;
        amount_micros: number;
        resource: string;
        scheme: AitpScheme;
        ttl_ms: number;
    }): Promise<PaymentChallenge>;
    /** Settle only after the payer's wallet/HSM signs the challenge intent. */
    settleAitp(authorization: PaymentAuthorization): Promise<AitpSettlement>;
    private request;
    private requestWithHeaders;
}
