/**
 * Agent-side AITP-402 payment client for Turnstile-gated endpoints.
 *
 * One paid call (HTTP or MCP):
 *  1. send the request unpaid;
 *  2. Turnstile answers `402` + `AITP-Challenge` (HTTP) or JSON-RPC error
 *     `-32402` with `data.aitp_challenge` (MCP);
 *  3. the client checks the challenge against its {@link SpendPolicy}, hashes
 *     the authorization intent (BLAKE3 over sorted-key canonical JSON, exactly
 *     like Mint) and signs the hash with the payer's Ed25519 key;
 *  4. it retries with `AITP-Authorization` (HTTP) or the reserved
 *     `_aitp_authorization` tool argument (MCP); Turnstile has Mint verify the
 *     signature and open the hold, runs the tool, captures actual usage
 *     (success-only) and returns the signed `AITP-Receipt`;
 *  5. {@link TurnstileClient.settlement} / `waitForSettlement` poll
 *     `GET /v1/settlements/:request_id` when a capture/release is pending.
 *
 * The client never calls Mint and never holds money. Runtime requirements:
 * `fetch` and WebCrypto Ed25519 (Node >= 20, Deno, Bun, modern browsers).
 */

import { blake3 } from "./blake3.ts";
import { b64urlDecode, b64urlEncode, hexDecode, hexEncode, importSeed, signEd25519 } from "./ed25519.ts";
import type { Ed25519Key } from "./ed25519.ts";

export const AITP_VERSION = "0.1";
export const MCP_PAYMENT_REQUIRED = -32402;
export const AUTHORIZATION_ARG = "_aitp_authorization";
const TERMINAL_PHASES = new Set(["captured", "released", "transfer_complete", "rejected"]);

export class TurnstileError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "TurnstileError";
  }
}

/** The spending policy refused to pay; nothing was signed. */
export class PaymentRefused extends TurnstileError {
  constructor(message: string) {
    super(message);
    this.name = "PaymentRefused";
  }
}

/** Turnstile rejected the signed authorization. */
export class PaymentFailed extends TurnstileError {
  readonly reason: string | undefined;
  constructor(message: string, reason?: string) {
    super(reason ? `${message}: ${reason}` : message);
    this.name = "PaymentFailed";
    this.reason = reason;
  }
}

export class HttpError extends TurnstileError {
  readonly status: number;
  readonly body: string;
  constructor(status: number, body: string) {
    super(`HTTP ${status}: ${body.slice(0, 300)}`);
    this.name = "HttpError";
    this.status = status;
    this.body = body;
  }
}

export interface Challenge {
  payee: string;
  amount_micros: number;
  currency: string;
  resource: string;
  nonce: string;
  expiry: number;
  scheme: string;
  /** The raw base64url header value. */
  encoded: string;
}

export interface Authorization {
  payer: string;
  payee: string;
  amount_micros: number;
  nonce: string;
  resource: string;
  expiry: number;
  scheme: string;
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
  balance_after_micros?: number;
  prev_hash?: string;
  hash: string;
  sig: string;
}

/** Anything that can sign a 32-byte op_hash for a key registered with Mint. */
export interface Signer {
  readonly pubkeyId: string;
  sign(opHash: Uint8Array<ArrayBuffer>): Promise<Uint8Array>;
}

/** Signs with a 32-byte Ed25519 seed. Register `publicKeyHex` with Mint under
 * `(payer, pubkeyId)` before paying. */
export class Ed25519Signer implements Signer {
  readonly pubkeyId: string;
  private readonly key: Ed25519Key;

  private constructor(pubkeyId: string, key: Ed25519Key) {
    this.pubkeyId = pubkeyId;
    this.key = key;
  }

  static async fromSeed(pubkeyId: string, seed: Uint8Array): Promise<Ed25519Signer> {
    if (!pubkeyId) throw new Error("pubkeyId is required");
    return new Ed25519Signer(pubkeyId, await importSeed(seed));
  }

  static async fromSeedHex(pubkeyId: string, seedHex: string): Promise<Ed25519Signer> {
    return Ed25519Signer.fromSeed(pubkeyId, hexDecode(seedHex));
  }

  get publicKeyHex(): string {
    return hexEncode(this.key.publicKey);
  }

  sign(opHash: Uint8Array<ArrayBuffer>): Promise<Uint8Array> {
    return signEd25519(this.key.privateKey, opHash);
  }
}

export interface SpendPolicyOptions {
  maxPriceMicros: number;
  allowedPayees?: string[];
  allowedCurrencies?: string[];
  totalBudgetMicros?: number;
}

/** What the agent is willing to pay; checked before anything is signed. */
export class SpendPolicy {
  readonly maxPriceMicros: number;
  readonly allowedPayees: Set<string> | undefined;
  readonly allowedCurrencies: Set<string>;
  readonly totalBudgetMicros: number | undefined;
  private spent = 0;

  constructor(opts: SpendPolicyOptions) {
    this.maxPriceMicros = opts.maxPriceMicros;
    this.allowedPayees = opts.allowedPayees ? new Set(opts.allowedPayees) : undefined;
    this.allowedCurrencies = new Set(opts.allowedCurrencies ?? ["AIT"]);
    this.totalBudgetMicros = opts.totalBudgetMicros;
  }

  get spentMicros(): number {
    return this.spent;
  }

  check(ch: Challenge): void {
    if (!(ch.amount_micros > 0)) throw new PaymentRefused("challenge quotes a non-positive amount");
    if (ch.amount_micros > this.maxPriceMicros) {
      throw new PaymentRefused(`price ${ch.amount_micros} micros exceeds the cap ${this.maxPriceMicros}`);
    }
    if (this.allowedPayees && !this.allowedPayees.has(ch.payee)) {
      throw new PaymentRefused(`payee ${ch.payee} is not allowed`);
    }
    if (!this.allowedCurrencies.has(ch.currency)) throw new PaymentRefused(`currency ${ch.currency} is not allowed`);
    if (ch.expiry <= Date.now()) throw new PaymentRefused("challenge already expired");
    if (this.totalBudgetMicros !== undefined && this.spent + ch.amount_micros > this.totalBudgetMicros) {
      throw new PaymentRefused("total budget exhausted");
    }
  }

  record(micros: number): void {
    this.spent += Math.max(0, micros);
  }
}

const textEncoder = new TextEncoder();
const textDecoder = new TextDecoder();

/** Sorted-key compact JSON, byte-compatible with Mint's canonical form. */
export function canonicalJson(value: Record<string, string | number>): string {
  const sorted: Record<string, string | number> = {};
  for (const key of Object.keys(value).sort()) sorted[key] = value[key];
  return JSON.stringify(sorted);
}

export function decodeChallenge(header: string): Challenge {
  let data: Record<string, unknown>;
  try {
    data = JSON.parse(textDecoder.decode(b64urlDecode(header)));
  } catch (err) {
    throw new TurnstileError(`malformed AITP-Challenge: ${(err as Error).message}`);
  }
  const need = ["payee", "amount_micros", "currency", "resource", "nonce", "expiry", "scheme"];
  for (const key of need) {
    if (data[key] === undefined) throw new TurnstileError(`malformed AITP-Challenge: missing ${key}`);
  }
  return {
    payee: String(data.payee),
    amount_micros: Number(data.amount_micros),
    currency: String(data.currency),
    resource: String(data.resource),
    nonce: String(data.nonce),
    expiry: Number(data.expiry),
    scheme: String(data.scheme),
    encoded: header.trim(),
  };
}

export function decodeReceipt(header: string | null | undefined): Receipt | undefined {
  if (!header) return undefined;
  try {
    return JSON.parse(textDecoder.decode(b64urlDecode(header))) as Receipt;
  } catch {
    return undefined;
  }
}

/** Build and sign the authorization for `challenge`. */
export async function buildAuthorization(payer: string, signer: Signer, ch: Challenge): Promise<Authorization> {
  const idempotency_key = `aitp:${ch.nonce}`;
  const intent = {
    payer,
    payee: ch.payee,
    amount_micros: ch.amount_micros,
    nonce: ch.nonce,
    resource: ch.resource,
    expiry: ch.expiry,
    scheme: ch.scheme,
    idempotency_key,
  };
  const opHash = blake3(textEncoder.encode(canonicalJson(intent)));
  const sig = await signer.sign(opHash);
  return { ...intent, pubkey_id: signer.pubkeyId, op_hash: hexEncode(opHash), sig: b64urlEncode(sig) };
}

export function encodeAuthorization(auth: Authorization): string {
  return b64urlEncode(textEncoder.encode(JSON.stringify(auth)));
}

export interface PaidResponse {
  status: number;
  headers: Headers;
  body: string;
  paid: boolean;
  challenge?: Challenge;
  /** Encoded `AITP-Authorization` (needed to read the settlement status). */
  authorization?: string;
  requestId?: string;
  receipt?: Receipt;
  /** `capture_pending` / `release_pending` when settlement is still in flight. */
  settlement?: string;
}

export interface McpResult {
  result: Record<string, unknown>;
  paid: boolean;
  receipt?: Receipt;
  authorization?: string;
  requestId?: string;
}

export interface SettlementStatus {
  request_id: string;
  phase: string;
  settled: boolean;
  scheme?: string;
  payer: string;
  payee: string;
  resource: string;
  authorized_micros: number;
  hold?: string;
  actual_micros?: number;
  receipt?: Receipt;
  error?: string;
}

export interface ToolDescriptor {
  name: string;
  resource: string;
  description: string;
  price_micros: number;
  scheme: string;
}

export interface ToolCatalog {
  aitp_version: string;
  payee: string;
  currency: string;
  tools: ToolDescriptor[];
}

export interface TurnstileClientOptions {
  baseUrl: string;
  /** The paying Mint account id. */
  payer: string;
  signer: Signer;
  policy: SpendPolicy;
  /** Per-request timeout in milliseconds (default 30000). */
  timeoutMs?: number;
  headers?: Record<string, string>;
  fetch?: typeof fetch;
}

/** Pays Turnstile-gated tools on behalf of one Mint payer account. */
export class TurnstileClient {
  readonly baseUrl: string;
  readonly payer: string;
  readonly policy: SpendPolicy;
  private readonly signer: Signer;
  private readonly timeoutMs: number;
  private readonly extraHeaders: Record<string, string>;
  private readonly fetchImpl: typeof fetch;

  constructor(opts: TurnstileClientOptions) {
    if (!opts.payer) throw new Error("payer (Mint account id) is required");
    this.baseUrl = opts.baseUrl.replace(/\/+$/, "");
    this.payer = opts.payer;
    this.signer = opts.signer;
    this.policy = opts.policy;
    this.timeoutMs = opts.timeoutMs ?? 30_000;
    this.extraHeaders = opts.headers ?? {};
    const f = opts.fetch ?? globalThis.fetch;
    if (!f) throw new Error("fetch is not available; pass options.fetch");
    this.fetchImpl = f;
  }

  /** `GET /v1/tools` — the provider's priced tool catalog (free). */
  async tools(): Promise<ToolCatalog> {
    const res = await this.send("GET", "/v1/tools", {});
    const body = await res.text();
    if (res.status !== 200) throw new HttpError(res.status, body);
    return JSON.parse(body) as ToolCatalog;
  }

  /** Send a request, paying one `402` challenge if the route is gated. */
  async request(method: string, path: string, body?: string | Uint8Array<ArrayBuffer>, headers: Record<string, string> = {}): Promise<PaidResponse> {
    const hdrs: Record<string, string> = { "aitp-version": AITP_VERSION, ...this.extraHeaders, ...headers };
    const first = await this.send(method, path, hdrs, body);
    if (first.status !== 402) {
      return { status: first.status, headers: first.headers, body: await first.text(), paid: false };
    }
    await first.arrayBuffer().catch(() => undefined);
    const encoded = first.headers.get("aitp-challenge");
    if (!encoded) {
      throw new TurnstileError(`402 without AITP-Challenge: ${first.headers.get("aitp-error") ?? "provider unavailable"}`);
    }
    const challenge = decodeChallenge(encoded);
    this.policy.check(challenge);
    const authorization = encodeAuthorization(await buildAuthorization(this.payer, this.signer, challenge));

    const paid = await this.send(method, path, { ...hdrs, "aitp-authorization": authorization }, body);
    const text = await paid.text();
    if (paid.status === 402) {
      throw new PaymentFailed("Turnstile rejected the payment", paid.headers.get("aitp-error") ?? undefined);
    }
    const receipt = decodeReceipt(paid.headers.get("aitp-receipt"));
    const settlement = paid.headers.get("aitp-settlement") ?? undefined;
    if (receipt) this.policy.record(Number(receipt.amount_micros ?? challenge.amount_micros));
    else if (paid.ok || settlement === "capture_pending") this.policy.record(challenge.amount_micros);
    return {
      status: paid.status,
      headers: paid.headers,
      body: text,
      paid: true,
      challenge,
      authorization,
      requestId: paid.headers.get("aitp-request-id") ?? challenge.nonce,
      receipt,
      settlement,
    };
  }

  /** `POST path` with a JSON body, paying on demand. */
  postJson(path: string, payload: unknown): Promise<PaidResponse> {
    return this.request("POST", path, JSON.stringify(payload), { "content-type": "application/json" });
  }

  /** `POST /v1/tools/<name>/call`, paying on demand. */
  callTool(name: string, args: unknown): Promise<PaidResponse> {
    return this.postJson(`/v1/tools/${encodeURIComponent(name)}/call`, args);
  }

  /** `GET /v1/settlements/:request_id`, scoped by the paid authorization. */
  async settlement(requestId: string, authorization: string): Promise<SettlementStatus> {
    const res = await this.send("GET", `/v1/settlements/${encodeURIComponent(requestId)}`, {
      "aitp-authorization": authorization,
    });
    const body = await res.text();
    if (res.status !== 200) throw new HttpError(res.status, body);
    return JSON.parse(body) as SettlementStatus;
  }

  /** Poll until the paid request's settlement reaches a terminal phase. */
  async waitForSettlement(response: PaidResponse, timeoutMs = 60_000, intervalMs = 1_000): Promise<SettlementStatus> {
    if (!response.paid || !response.requestId || !response.authorization) {
      throw new TurnstileError("response was not a paid request");
    }
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      try {
        const status = await this.settlement(response.requestId, response.authorization);
        if (status.settled || TERMINAL_PHASES.has(status.phase)) return status;
      } catch (err) {
        if (!(err instanceof HttpError) || (err.status !== 404 && err.status !== 503)) throw err;
      }
      if (Date.now() >= deadline) {
        throw new TurnstileError(`settlement ${response.requestId} not terminal after ${timeoutMs} ms`);
      }
      await new Promise((resolve) => setTimeout(resolve, intervalMs));
    }
  }

  /** Raw JSON-RPC call to `POST /mcp` (returns the whole envelope). */
  async mcp(method: string, params?: Record<string, unknown>, id: number | string = 1): Promise<Record<string, any>> {
    const payload: Record<string, unknown> = { jsonrpc: "2.0", id, method };
    if (params !== undefined) payload.params = params;
    const res = await this.send("POST", "/mcp", { "content-type": "application/json" }, JSON.stringify(payload));
    const body = await res.text();
    if (res.status !== 200) throw new HttpError(res.status, body);
    return JSON.parse(body);
  }

  async mcpTools(): Promise<Array<Record<string, unknown>>> {
    const env = await this.mcp("tools/list", {});
    if (env.error) throw new TurnstileError(`tools/list failed: ${env.error.message ?? JSON.stringify(env.error)}`);
    return (env.result?.tools ?? []) as Array<Record<string, unknown>>;
  }

  /**
   * MCP `tools/call`, paying the AITP challenge when required. The challenge
   * arrives as a `-32402` JSON-RPC error, or (on an x402-enabled server) as an
   * x402 `PaymentRequired` tool result carrying it in `_meta["aitp/challenge"]`.
   */
  async mcpCall(name: string, args: Record<string, unknown> = {}): Promise<McpResult> {
    const first = await this.mcp("tools/call", { name, arguments: { ...args } });
    const x402Challenge = first.result?.isError ? first.result?._meta?.["aitp/challenge"] : undefined;
    if (first.result && !x402Challenge) return { result: first.result, paid: false };
    const error = x402Challenge ? { code: MCP_PAYMENT_REQUIRED, data: x402Challenge } : (first.error ?? {});
    if (error.code !== MCP_PAYMENT_REQUIRED) {
      throw new TurnstileError(`tools/call ${name} failed: ${error.message ?? JSON.stringify(error)}`);
    }
    const encoded: string | undefined = error.data?.aitp_challenge;
    if (!encoded) {
      throw new TurnstileError(`payment required without a challenge: ${error.data?.aitp_error ?? "provider unavailable"}`);
    }
    const challenge = decodeChallenge(encoded);
    this.policy.check(challenge);
    const authorization = encodeAuthorization(await buildAuthorization(this.payer, this.signer, challenge));
    const paid = await this.mcp("tools/call", { name, arguments: { ...args, [AUTHORIZATION_ARG]: authorization } }, 2);
    if (paid.error) {
      if (paid.error.code === MCP_PAYMENT_REQUIRED) {
        throw new PaymentFailed("Turnstile rejected the payment", paid.error.data?.aitp_error);
      }
      throw new TurnstileError(`tools/call ${name} failed: ${paid.error.message ?? JSON.stringify(paid.error)}`);
    }
    const result = (paid.result ?? {}) as Record<string, any>;
    const receipt = decodeReceipt(result._meta?.aitp_receipt);
    this.policy.record(receipt ? Number(result._meta?.amount_micros ?? challenge.amount_micros) : challenge.amount_micros);
    return { result, paid: true, receipt, authorization, requestId: challenge.nonce };
  }

  private async send(method: string, path: string, headers: Record<string, string>, body?: string | Uint8Array<ArrayBuffer>): Promise<Response> {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    try {
      return await this.fetchImpl(this.baseUrl + path, { method, headers, body, signal: controller.signal });
    } catch (err) {
      throw new TurnstileError(`request to ${this.baseUrl + path} failed: ${(err as Error).message}`);
    } finally {
      clearTimeout(timer);
    }
  }
}
