export const AITP_VERSION = "0.1";
export const HEADER_VERSION = "AITP-Version";
export const HEADER_CHALLENGE = "AITP-Challenge";
export const HEADER_AUTHORIZATION = "AITP-Authorization";
export const HEADER_RECEIPT = "AITP-Receipt";

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

export class AuthorizationBuilder {
  constructor(
    readonly payer: string,
    private readonly signer: WalletSigner,
  ) {}

  async authorize(challenge: PaymentChallenge): Promise<PaymentAuthorization> {
    validateChallenge(challenge);
    if (challenge.expiry <= Date.now()) throw new Error("AITP challenge is expired");
    const idempotencyKey = `aitp:${challenge.nonce}`;
    const intent: Record<string, string | number> = {
      payer: this.payer,
      payee: challenge.payee,
      amount_micros: challenge.amount_micros,
      nonce: challenge.nonce,
      resource: challenge.resource,
      expiry: challenge.expiry,
      scheme: challenge.scheme,
      idempotency_key: idempotencyKey,
    };
    const sorted = Object.fromEntries(
      Object.entries(intent).sort(([left], [right]) => left.localeCompare(right)),
    );
    const signed = await this.signer.signAuthorization(
      new TextEncoder().encode(JSON.stringify(sorted)),
    );
    if (!/^[0-9a-f]{64}$/.test(signed.op_hash)) {
      throw new Error("wallet signer returned a non-canonical BLAKE3 hex digest");
    }
    if (!signed.pubkey_id || !signed.sig) {
      throw new Error("wallet signer returned an empty key id or signature");
    }
    return {
      payer: this.payer,
      payee: challenge.payee,
      amount_micros: challenge.amount_micros,
      nonce: challenge.nonce,
      resource: challenge.resource,
      expiry: challenge.expiry,
      scheme: challenge.scheme,
      idempotency_key: idempotencyKey,
      pubkey_id: signed.pubkey_id,
      op_hash: signed.op_hash,
      sig: signed.sig,
    };
  }
}

export interface HttpRequest {
  method: string;
  url: string;
  headers?: HeadersInit;
  body?: BodyInit | null;
}

export class PaidHttpClient {
  private readonly fetcher: typeof globalThis.fetch;

  constructor(
    private readonly authorizer: AuthorizationBuilder,
    fetcher: typeof globalThis.fetch = globalThis.fetch,
  ) {
    if (!fetcher) throw new Error("PaidHttpClient requires a Fetch API implementation");
    this.fetcher = fetcher;
  }

  /** Send once, then sign and retry exactly once for a valid AITP 402. */
  async send(request: HttpRequest): Promise<Response> {
    const body = await replayableBody(request.body);
    const first = await this.fetcher(request.url, {
      method: request.method,
      ...(request.headers === undefined ? {} : { headers: request.headers }),
      ...(body === undefined ? {} : { body }),
      redirect: "manual",
    });
    if (first.status !== 402) return first;
    const encoded = first.headers.get(HEADER_CHALLENGE);
    if (!encoded) throw new Error("402 response missing AITP-Challenge header");
    const authorization = await this.authorizer.authorize(decodeChallenge(encoded));
    const headers = new Headers(request.headers);
    headers.set(HEADER_VERSION, AITP_VERSION);
    headers.set(HEADER_AUTHORIZATION, encodeAuthorization(authorization));
    return this.fetcher(request.url, {
      method: request.method,
      headers,
      ...(body === undefined ? {} : { body }),
      redirect: "manual",
    });
  }
}

async function replayableBody(body: BodyInit | null | undefined): Promise<BodyInit | undefined> {
  if (body === undefined || body === null) return undefined;
  if (body instanceof ReadableStream) {
    throw new Error("AITP retry requires a replayable request body, not a ReadableStream");
  }
  if (body instanceof FormData) {
    throw new Error("AITP retry requires stable bytes; pre-encode FormData before sending");
  }
  return body;
}

export function encodeAuthorization(value: PaymentAuthorization): string {
  return encodeHeader(value);
}

export function decodeAuthorization(header: string): PaymentAuthorization {
  return decodeHeader(header) as unknown as PaymentAuthorization;
}

export function decodeChallenge(header: string): PaymentChallenge {
  const value = decodeHeader(header);
  const challenge = value as unknown as PaymentChallenge;
  validateChallenge(challenge);
  return challenge;
}

export function decodeReceipt(header: string): Receipt {
  return decodeHeader(header) as unknown as Receipt;
}

function encodeHeader(value: object): string {
  const bytes = new TextEncoder().encode(JSON.stringify(value));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

function decodeHeader(header: string): Record<string, unknown> {
  const base64 = header.trim().replaceAll("-", "+").replaceAll("_", "/");
  const padded = base64 + "=".repeat((4 - (base64.length % 4)) % 4);
  const binary = atob(padded);
  const bytes = Uint8Array.from(binary, (character) => character.charCodeAt(0));
  const value: unknown = JSON.parse(new TextDecoder().decode(bytes));
  if (!isRecord(value)) throw new Error("AITP header must encode a JSON object");
  return value;
}

function validateChallenge(value: PaymentChallenge): void {
  if (!isRecord(value)) throw new Error("AITP challenge must be an object");
  if (value.currency !== "AIT") throw new Error("AITP currency must be AIT");
  if (value.scheme !== "aitp-hold" && value.scheme !== "aitp-transfer") {
    throw new Error("unsupported AITP scheme");
  }
  if (!Number.isSafeInteger(value.amount_micros) || value.amount_micros < 0) {
    throw new Error("AITP amount_micros must be a non-negative safe integer");
  }
  if (!Number.isSafeInteger(value.expiry)) throw new Error("AITP expiry must be a safe integer");
  if (!value.payee || !value.resource || !value.nonce) {
    throw new Error("AITP challenge identity fields cannot be empty");
  }
}

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

export interface CloseStreamRequest extends TickStreamRequest {}

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
  payout_dest:
    | { kind: "stripe"; account: string }
    | { kind: "crypto"; chain: string; address: string }
    | { kind: "paypal"; email: string };
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

export class MintApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
    readonly body: unknown,
    message: string,
  ) {
    super(message);
    this.name = "MintApiError";
  }
}

export class MintClient {
  readonly baseUrl: string;
  private readonly apiKey: string;
  private readonly fetcher: typeof globalThis.fetch;

  constructor(options: MintClientOptions) {
    const url = new URL(options.baseUrl);
    if (url.protocol !== "http:" && url.protocol !== "https:") {
      throw new Error("baseUrl must be an absolute HTTP(S) URL");
    }
    this.baseUrl = options.baseUrl.replace(/\/+$/, "") + "/";
    this.apiKey = options.apiKey;
    this.fetcher = options.fetch ?? globalThis.fetch;
    if (!this.fetcher) throw new Error("MintClient requires a Fetch API implementation");
  }

  createAccount(kind: string, ownerRef?: string): Promise<Record<string, unknown>> {
    return this.request("POST", "v1/accounts", { kind, owner_ref: ownerRef ?? null });
  }

  account(accountId: string): Promise<Record<string, unknown>> {
    return this.request("GET", `v1/accounts/${segment(accountId)}`);
  }

  balance(accountId: string): Promise<Record<string, unknown>> {
    return this.request("GET", `v1/accounts/${segment(accountId)}/balance`);
  }

  createAddress(accountId: string, purpose?: string): Promise<CreateAddressResponse> {
    return this.request("POST", `v1/accounts/${segment(accountId)}/addresses`, {
      purpose: purpose ?? null,
    });
  }

  lookupAddress(address: string): Promise<AddressLookup> {
    return this.request("GET", `v1/addresses/${segment(address)}`);
  }

  createCheckout(provider: string, accountId: string, amountMicros: number): Promise<Record<string, unknown>> {
    return this.request("POST", "v1/checkouts", {
      provider,
      account_id: accountId,
      amount_micros: amountMicros,
    });
  }

  /**
   * Move integer microdollars exactly once for one logical operation.
   * Keep this account-id API and the Mint service key behind your backend.
   */
  transfer(input: TransferRequest): Promise<TransferReceipt> {
    validateTransfer(input);
    if (input.to.startsWith("ait1")) {
      throw new Error(
        "public Mint addresses must use transferToAddress so the signed receipt can be bound to the resolved recipient",
      );
    }
    return this.request<Record<string, unknown>>("POST", "v1/transfers", input).then((value) => {
      validateTransferReceipt(value, input);
      return value as unknown as TransferReceipt;
    });
  }

  /** Resolve a public receive address, then bind the signed proof to its account. */
  async transferToAddress(
    input: Omit<TransferRequest, "to"> & { address: string },
  ): Promise<TransferReceipt> {
    if (!input.address.startsWith("ait1")) {
      throw new Error("public Mint addresses must begin with ait1");
    }
    const recipient = await this.lookupAddress(input.address);
    if (typeof recipient.account_id !== "string" || recipient.account_id.length === 0) {
      throw new MintApiError(200, "invalid_response", recipient, "Mint address lookup is invalid");
    }
    const { address: _address, ...transfer } = input;
    return this.transfer({ ...transfer, to: recipient.account_id });
  }

  createHold(input: Record<string, unknown>): Promise<Record<string, unknown>> {
    return this.request("POST", "v1/holds", input);
  }

  hold(holdId: string): Promise<Record<string, unknown>> {
    return this.request("GET", `v1/holds/${segment(holdId)}`);
  }

  captureHold(holdId: string, input: Record<string, unknown>): Promise<Record<string, unknown>> {
    return this.request("POST", `v1/holds/${segment(holdId)}/capture`, input);
  }

  releaseHold(holdId: string, input: Record<string, unknown>): Promise<Record<string, unknown>> {
    return this.request("POST", `v1/holds/${segment(holdId)}/release`, input);
  }

  grantAllowance(input: Record<string, unknown>): Promise<Record<string, unknown>> {
    return this.request("POST", "v1/allowances", input);
  }

  allowance(allowanceId: string): Promise<Record<string, unknown>> {
    return this.request("GET", `v1/allowances/${segment(allowanceId)}`);
  }

  spendAllowance(allowanceId: string, input: Record<string, unknown>): Promise<Record<string, unknown>> {
    return this.request("POST", `v1/allowances/${segment(allowanceId)}/spend`, input);
  }

  revokeAllowance(allowanceId: string, idemKey: string): Promise<{ revoked: boolean }> {
    return this.request("POST", `v1/allowances/${segment(allowanceId)}/revoke`, {
      idem_key: idemKey,
    });
  }

  openStream(input: OpenStreamRequest): Promise<Record<string, unknown>> {
    validatePositiveMoney("stream rate_micros", input.rate_micros);
    validatePositiveMoney("stream cap_micros", input.cap_micros);
    return this.request("POST", "v1/streams", input);
  }

  stream(streamId: string): Promise<Record<string, unknown>> {
    return this.request("GET", `v1/streams/${segment(streamId)}`);
  }

  tickStream(streamId: string, input: TickStreamRequest): Promise<Record<string, unknown>> {
    validateCounter("stream units", input.units);
    validateCounter("stream seq", input.seq);
    return this.request("POST", `v1/streams/${segment(streamId)}/tick`, input);
  }

  closeStream(streamId: string, input: CloseStreamRequest): Promise<Record<string, unknown>> {
    validateCounter("stream units", input.units);
    validateCounter("stream seq", input.seq);
    return this.request("POST", `v1/streams/${segment(streamId)}/close`, input);
  }

  accountReceipts(accountId: string): Promise<SignedTransferReceipt[]> {
    return this.request("GET", `v1/accounts/${segment(accountId)}/receipts`);
  }

  receiptAudit(): Promise<{ merkle_root: string; epoch: number }> {
    return this.request("GET", "v1/receipts/audit");
  }

  reserve(): Promise<ReserveProof> {
    return this.request("GET", "v1/reserve");
  }

  mint(input: MintRequest): Promise<Record<string, unknown>> {
    validatePositiveMoney("mint amount_micros", input.amount_micros);
    return this.request("POST", "v1/mint", input);
  }

  redeem(input: RedeemRequest): Promise<Record<string, unknown>> {
    validatePositiveMoney("redeem amount_micros", input.amount_micros);
    return this.request("POST", "v1/redeem", input);
  }

  /** Operator listing of accounts with live balances (keyset paginated). */
  listAccounts(params: { after?: string; limit?: number } = {}): Promise<Page<AccountSummary>> {
    return this.request("GET", `v1/accounts${query(params)}`);
  }

  /** Period statement: opening balance, entries, totals, closing balance. */
  statement(
    accountId: string,
    params: { from_ms?: number; to_ms?: number } = {},
  ): Promise<Record<string, unknown>> {
    return this.request("GET", `v1/accounts/${segment(accountId)}/statement${query(params)}`);
  }

  kycStatus(accountId: string): Promise<Record<string, unknown>> {
    return this.request("GET", `v1/accounts/${segment(accountId)}/kyc`);
  }

  startKycSession(accountId: string): Promise<{ provider: string; url: string } & Record<string, unknown>> {
    return this.request("POST", `v1/accounts/${segment(accountId)}/kyc/session`, {});
  }

  /** Block outgoing movement from an account (operator key). */
  freezeAccount(accountId: string): Promise<AccountSummary> {
    return this.request("POST", `v1/accounts/${segment(accountId)}/freeze`, {});
  }

  unfreezeAccount(accountId: string): Promise<AccountSummary> {
    return this.request("POST", `v1/accounts/${segment(accountId)}/unfreeze`, {});
  }

  /** Permanently close a zero-balance account (operator key). */
  closeAccount(accountId: string): Promise<AccountSummary> {
    return this.request("POST", `v1/accounts/${segment(accountId)}/close`, {});
  }

  listPayouts(
    params: { status?: PayoutStatus; account_id?: string; limit?: number; cursor?: string } = {},
  ): Promise<Page<PayoutRecord>> {
    return this.request("GET", `v1/payouts${query(params)}`);
  }

  /** One payout; a pending payout is refreshed from the provider first. */
  payout(payoutRef: string): Promise<PayoutRecord> {
    return this.request("GET", `v1/payouts/${segment(payoutRef)}`);
  }

  retryPayout(payoutRef: string): Promise<PayoutRecord> {
    return this.request("POST", `v1/payouts/${segment(payoutRef)}/retry`, {});
  }

  /** Abandon a failed payout and re-credit the burned amount (operator key). */
  cancelPayout(payoutRef: string, note?: string): Promise<PayoutRecord> {
    return this.request("POST", `v1/payouts/${segment(payoutRef)}/cancel`, { note: note ?? null });
  }

  /** Record an out-of-band settlement of a payout (operator key). */
  markPayoutPaid(payoutRef: string, providerRef?: string): Promise<PayoutRecord> {
    return this.request("POST", `v1/payouts/${segment(payoutRef)}/mark-paid`, {
      provider_ref: providerRef ?? null,
    });
  }

  reconciliation(): Promise<Record<string, unknown>> {
    return this.request("GET", "v1/reconciliation");
  }

  /** Import a provider settlement report and get its match results. */
  importProviderReport(input: ImportReportRequest): Promise<Record<string, unknown>> {
    if (!input.content) throw new Error("report content cannot be empty");
    return this.request("POST", "v1/reconciliation/reports", input);
  }

  providerReports(): Promise<{ items: Record<string, unknown>[] }> {
    return this.request("GET", "v1/reconciliation/reports");
  }

  providerReport(reportId: string): Promise<Record<string, unknown>> {
    return this.request("GET", `v1/reconciliation/reports/${segment(reportId)}`);
  }

  deleteProviderReport(reportId: string): Promise<{ deleted: string }> {
    return this.request("DELETE", `v1/reconciliation/reports/${segment(reportId)}`);
  }

  /** Record a refund / chargeback and claw it back (operator key). */
  recordReversal(input: {
    deposit_ref: string;
    amount_micros?: number;
    reason?: "refund" | "dispute";
    note?: string;
  }): Promise<DepositReversal> {
    if (input.amount_micros !== undefined) {
      validatePositiveMoney("reversal amount_micros", input.amount_micros);
    }
    return this.request("POST", "v1/reversals", input);
  }

  reversals(params: { status?: ReversalStatus; limit?: number } = {}): Promise<{ items: DepositReversal[] }> {
    return this.request("GET", `v1/reversals${query(params)}`);
  }

  retryReversal(depositRef: string): Promise<DepositReversal> {
    return this.request("POST", `v1/reversals/${segment(depositRef)}/retry`, {});
  }

  writeOffReversal(depositRef: string, note?: string): Promise<DepositReversal> {
    return this.request("POST", `v1/reversals/${segment(depositRef)}/write-off`, { note: note ?? null });
  }

  /** DB-backed API keys (operator key). */
  apiKeys(): Promise<{ items: ApiKey[]; env_keys: Record<string, unknown>[] }> {
    return this.request("GET", "v1/keys");
  }

  createApiKey(input: { name: string; scopes: ApiScope[]; accounts?: string[] }): Promise<CreatedApiKey> {
    return this.request("POST", "v1/keys", input);
  }

  revokeApiKey(keyId: string): Promise<ApiKey> {
    return this.request("POST", `v1/keys/${segment(keyId)}/revoke`, {});
  }

  verificationKeys(): Promise<{ keys: VerificationKey[] }> {
    return this.request("GET", ".well-known/mint-verification-keys.json");
  }

  /** Issue the single-use 402 challenge that the payer signs locally. */
  async issueAitpChallenge(input: {
    payee: string;
    amount_micros: number;
    resource: string;
    scheme: AitpScheme;
    ttl_ms: number;
  }): Promise<PaymentChallenge> {
    validatePositiveMoney("challenge amount_micros", input.amount_micros);
    validatePositiveMoney("challenge ttl_ms", input.ttl_ms);
    const response = await this.fetcher(new URL("v1/aitp/challenge", this.baseUrl), {
      method: "POST",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "X-API-Key": this.apiKey,
      },
      body: JSON.stringify(input),
    });
    const payload: unknown = await response.json().catch(() => null);
    if (response.status !== 402 || !isRecord(payload)) {
      throw apiError(response.status, payload);
    }
    const encoded = response.headers.get(HEADER_CHALLENGE);
    if (!encoded) throw new MintApiError(402, "invalid_response", payload, "Mint challenge header is missing");
    const header = decodeChallenge(encoded);
    const body = payload as unknown as PaymentChallenge;
    validateChallenge(body);
    if (!sameChallenge(header, body)) {
      throw new MintApiError(402, "invalid_response", payload, "Mint challenge header and body disagree");
    }
    return body;
  }

  /** Settle only after the payer's wallet/HSM signs the challenge intent. */
  async settleAitp(authorization: PaymentAuthorization): Promise<AitpSettlement> {
    const { data, headers } = await this.requestWithHeaders<
      Omit<AitpSettlement, "signed_receipt">
    >("POST", "v1/aitp/verify", authorization);
    const encoded = headers.get(HEADER_RECEIPT);
    if (!encoded) {
      throw new MintApiError(200, "invalid_response", data, "Mint settlement receipt is missing");
    }
    return { ...data, signed_receipt: decodeReceipt(encoded) };
  }

  private async request<T = Record<string, unknown>>(
    method: string,
    path: string,
    body?: object,
  ): Promise<T> {
    return (await this.requestWithHeaders<T>(method, path, body)).data;
  }

  private async requestWithHeaders<T = Record<string, unknown>>(
    method: string,
    path: string,
    body?: object,
  ): Promise<{ data: T; headers: Headers }> {
    const headers = new Headers({ Accept: "application/json", "X-API-Key": this.apiKey });
    if (body !== undefined) headers.set("Content-Type", "application/json");
    const response = await this.fetcher(new URL(path, this.baseUrl), {
      method,
      headers,
      ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    });
    const payload: unknown = await response.json().catch(() => null);
    const data = isRecord(payload) ? payload.data : undefined;
    if (
      !response.ok
      || !isRecord(payload)
      || payload.success !== true
      || (!isRecord(data) && !Array.isArray(data))
    ) {
      throw apiError(response.status, payload);
    }
    return { data: data as T, headers: response.headers };
  }
}

function query(params: Record<string, string | number | undefined | null>): string {
  const qs = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== null && value !== "") qs.set(key, String(value));
  }
  const text = qs.toString();
  return text ? `?${text}` : "";
}

function apiError(status: number, payload: unknown): MintApiError {
  const detail = isRecord(payload) && isRecord(payload.error) ? payload.error : null;
  const code = detail && typeof detail.code === "string" ? detail.code : "http_error";
  const message = detail && typeof detail.message === "string"
    ? detail.message
    : `Mint request failed with HTTP ${status}`;
  return new MintApiError(status, code, payload, message);
}

function sameChallenge(left: PaymentChallenge, right: PaymentChallenge): boolean {
  return left.payee === right.payee
    && left.amount_micros === right.amount_micros
    && left.currency === right.currency
    && left.resource === right.resource
    && left.nonce === right.nonce
    && left.expiry === right.expiry
    && left.scheme === right.scheme;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function segment(value: string): string {
  return encodeURIComponent(value);
}

function validateTransfer(input: TransferRequest): void {
  for (const [name, value] of [
    ["from", input.from],
    ["to", input.to],
    ["idem_key", input.idem_key],
  ] as const) {
    if (typeof value !== "string" || value.trim().length === 0) {
      throw new Error(`transfer ${name} must be a non-empty string`);
    }
  }
  if (!Number.isSafeInteger(input.amount_micros) || input.amount_micros <= 0) {
    throw new Error("transfer amount_micros must be a positive safe integer");
  }
  if (input.idem_key.length > 128) {
    throw new Error("transfer idem_key must be at most 128 characters");
  }
  if (input.ref !== undefined && typeof input.ref !== "string") {
    throw new Error("transfer ref must be a string when provided");
  }
}

function validatePositiveMoney(name: string, value: number): void {
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${name} must be a positive safe integer`);
  }
}

function validateCounter(name: string, value: number): void {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${name} must be a non-negative safe integer`);
  }
}

function validateTransferReceipt(
  value: Record<string, unknown>,
  input: TransferRequest,
): void {
  const receipt = value.receipt;
  const attestation = value.attestation;
  if (
    !Number.isSafeInteger(value.first_seq) || (value.first_seq as number) < 0
    || !isRecord(value.from_balance)
    || !isRecord(value.to_balance)
    || typeof value.idempotent_replay !== "boolean"
    || !isRecord(receipt)
    || !isRecord(attestation)
    || !isRecord(receipt.body)
  ) {
    throw new MintApiError(200, "invalid_response", value, "Mint transfer proof is incomplete");
  }

  const body = receipt.body;
  const requiredStrings = [
    receipt.hash,
    receipt.sig,
    body.op,
    body.prev_hash,
    body.signer,
    attestation.payload_base64url,
    attestation.digest_sha256,
    attestation.signer_key_id,
    attestation.signature_hex,
  ];
  if (requiredStrings.some((item) => typeof item !== "string" || item.length === 0)) {
    throw new MintApiError(200, "invalid_response", value, "Mint transfer proof fields are invalid");
  }
  if (attestation.format !== "mint-transfer-attestation-v1") {
    throw new MintApiError(200, "invalid_response", value, "Mint transfer attestation format is invalid");
  }
  if (
    body.account_id !== input.from
    || body.from !== input.from
    || body.to !== input.to
    || body.journal_seq !== value.first_seq
    || body.amount_micros !== input.amount_micros
    || (body.op !== "Transfer" && body.op !== "transfer")
  ) {
    throw new MintApiError(200, "invalid_response", value, "Mint transfer proof does not match request");
  }
}
