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
    };
    idem_key: string;
}
export interface VerificationKey {
    key_id: string;
    algorithm: string;
    public_key_hex: string;
    status: string;
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
    verificationKeys(): Promise<{
        keys: VerificationKey[];
    }>;
    private request;
}
