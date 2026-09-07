import {
  AITP_AUTHORIZATION_HEADER,
  AITP_REQUEST_ID_HEADER,
} from "./aitp.js";
import {
  AlreadySettledError,
  errorFromResponse,
  InvalidSettlementEvidenceError,
  InsufficientCreditsError,
  PaymentRequiredError,
  UnverifiedSettlementEvidenceError,
} from "./errors.js";
import { parseChatCompletionStream } from "./stream.js";
import { MultipartForm } from "./multipart.js";
import type {
  AitpAuthorizer,
  CatalogSnapshot,
  ChatCompletion,
  ChatCompletionChunk,
  ChatCompletionParams,
  CostEventListQuery,
  CostEventPage,
  CreditStatus,
  DataEnvelope,
  GatewayHealth,
  GatewayStatus,
  PendingCostEvent,
  PriceQuote,
  PriceQuoteRequest,
  RelayModel,
  SettlementEvidence,
  SettlementEvidenceVerifier,
} from "./types.js";

export type FetchLike = (
  input: string,
  init?: {
    method?: string;
    headers?: Record<string, string>;
    body?: string | Uint8Array;
    signal?: AbortSignal;
    redirect?: RequestRedirect;
  },
) => Promise<FetchLikeResponse>;

export interface FetchLikeResponse {
  ok: boolean;
  status: number;
  headers: { get(name: string): string | null };
  json(): Promise<unknown>;
  text(): Promise<string>;
  body: ReadableStream<Uint8Array> | null;
}

export interface RelayClientOptions {
  baseUrl: string;
  apiKey?: string;
  fetch?: FetchLike;
  /** Backward-compatible local model list; prefer catalog(). */
  knownModels?: string[];
  timeoutMs?: number;
  maxResponseBytes?: number;
  maxSseEventChars?: number;
  /** Synchronous handoff only; perform evidence lookup in a separate worker. */
  onAbandonedStream?: (requestId: string) => void;
  /** Required before a 409 replay can be treated as cryptographic settlement. */
  settlementEvidenceVerifier?: SettlementEvidenceVerifier;
}

const PATHS = {
  status: "status",
  health: "health",
  catalog: "v1/relay/catalog",
  quotes: "v1/relay/quotes",
  chat: "openai/v1/chat/completions",
  embeddings: "openai/v1/embeddings",
  inference: "inference",
  batch: "batch_inference",
  costEvents: "v1/billing/cost-events",
} as const;

interface RequestOptions {
  method?: "GET" | "POST";
  accept?: string;
  headers?: Record<string, string>;
  acceptedErrorStatus?: number;
}

export class RelayClient {
  private readonly baseUrl: string;
  private readonly apiKey?: string;
  private readonly fetchImpl: FetchLike;
  private readonly knownModels: string[];
  private readonly timeoutMs?: number;
  private readonly maxResponseBytes: number;
  private readonly maxSseEventChars: number;
  private readonly onAbandonedStream?: (requestId: string) => void;
  private readonly settlementEvidenceVerifier?: SettlementEvidenceVerifier;

  constructor(options: RelayClientOptions) {
    if (!options || typeof options.baseUrl !== "string") {
      throw new Error("RelayClient requires a non-empty baseUrl");
    }
    this.baseUrl = normalizeBaseUrl(options.baseUrl);
    this.apiKey = validateOptionalHeader(options.apiKey, "Relay API key");
    this.knownModels = [...(options.knownModels ?? [])];
    this.timeoutMs = validateOptionalPositiveInteger(options.timeoutMs, "timeoutMs");
    this.maxResponseBytes =
      validateOptionalPositiveInteger(options.maxResponseBytes, "maxResponseBytes") ??
      16 * 1024 * 1024;
    this.maxSseEventChars =
      validateOptionalPositiveInteger(options.maxSseEventChars, "maxSseEventChars") ??
      2 * 1024 * 1024;
    this.onAbandonedStream = options.onAbandonedStream;
    this.settlementEvidenceVerifier = options.settlementEvidenceVerifier;

    if (options.fetch) {
      this.fetchImpl = options.fetch;
    } else if (typeof globalThis.fetch === "function") {
      this.fetchImpl = globalThis.fetch.bind(globalThis) as unknown as FetchLike;
    } else {
      throw new Error(
        "No fetch implementation available. Pass options.fetch or run on Node >= 18.",
      );
    }
  }

  async status(): Promise<GatewayStatus> {
    return this.json<GatewayStatus>(PATHS.status, undefined, { method: "GET" });
  }

  async health(): Promise<GatewayHealth> {
    return this.json<GatewayHealth>(PATHS.health, undefined, {
      method: "GET",
      acceptedErrorStatus: 503,
    });
  }

  async catalog(): Promise<CatalogSnapshot> {
    const envelope = await this.json<DataEnvelope<CatalogSnapshot>>(
      PATHS.catalog,
      undefined,
      { method: "GET" },
    );
    return envelope.data;
  }

  async quote(request: PriceQuoteRequest): Promise<PriceQuote> {
    validateTokenCount(request.input_tokens, "input_tokens");
    validateTokenCount(request.output_tokens, "output_tokens");
    if (!request.model?.trim()) throw new Error("quote() requires a model");
    const envelope = await this.json<DataEnvelope<PriceQuote>>(
      PATHS.quotes,
      request,
    );
    return envelope.data;
  }

  async chatCompletions(params: ChatCompletionParams): Promise<ChatCompletion> {
    validateChatParams(params);
    return this.json<ChatCompletion>(PATHS.chat, { ...params, stream: false });
  }

  async paidChatCompletions(
    params: ChatCompletionParams,
    authorizer: AitpAuthorizer,
  ): Promise<ChatCompletion> {
    validateChatParams(params);
    const response = await this.paidResponse(
      PATHS.chat,
      { ...params, stream: false },
      authorizer,
    );
    return this.decodeJson<ChatCompletion>(response);
  }

  async *chatCompletionsStream(
    params: ChatCompletionParams,
  ): AsyncGenerator<ChatCompletionChunk, void, unknown> {
    yield* this.openChatStream(params);
  }

  async *paidChatCompletionsStream(
    params: ChatCompletionParams,
    authorizer: AitpAuthorizer,
  ): AsyncGenerator<ChatCompletionChunk, void, unknown> {
    yield* this.openChatStream(params, authorizer);
  }

  async embeddings<T = unknown>(body: unknown): Promise<T> {
    return this.json<T>(PATHS.embeddings, body);
  }

  async paidEmbeddings<T = unknown>(
    body: unknown,
    authorizer: AitpAuthorizer,
  ): Promise<T> {
    return this.decodeJson<T>(
      await this.paidResponse(PATHS.embeddings, body, authorizer),
    );
  }

  async inference<T = unknown>(body: unknown): Promise<T> {
    return this.json<T>(PATHS.inference, body);
  }

  async paidInference<T = unknown>(
    body: unknown,
    authorizer: AitpAuthorizer,
  ): Promise<T> {
    return this.decodeJson<T>(
      await this.paidResponse(PATHS.inference, body, authorizer),
    );
  }

  async batchInference<T = unknown>(body: unknown): Promise<T> {
    return this.json<T>(PATHS.batch, body);
  }

  async paidBatchInference<T = unknown>(
    body: unknown,
    authorizer: AitpAuthorizer,
  ): Promise<T> {
    return this.decodeJson<T>(
      await this.paidResponse(PATHS.batch, body, authorizer),
    );
  }

  async batchInferenceStatus<T = unknown>(batchId: string): Promise<T> {
    validateUuid(batchId, "batch id");
    return this.json<T>(`${PATHS.batch}/${batchId}`, undefined, { method: "GET" });
  }

  async settlementEvidence(requestId: string): Promise<SettlementEvidence> {
    const evidence = await this.settlementEvidenceUnverified(requestId);
    if (!this.settlementEvidenceVerifier) {
      throw new UnverifiedSettlementEvidenceError(evidence);
    }
    try {
      if (await this.settlementEvidenceVerifier(evidence)) return evidence;
    } catch {
      // Verifier errors fail closed like an invalid signature.
    }
    throw new InvalidSettlementEvidenceError(evidence);
  }

  async settlementEvidenceUnverified(requestId: string): Promise<SettlementEvidence> {
    validateUuid(requestId, "request id");
    const envelope = await this.json<DataEnvelope<SettlementEvidence>>(
      `v1/billing/settlements/${requestId}`,
      undefined,
      { method: "GET" },
    );
    return envelope.data;
  }

  async costEvents(query: CostEventListQuery = {}): Promise<CostEventPage> {
    const limit = query.limit ?? 50;
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > 100) {
      throw new Error("costEvents() limit must be between 1 and 100");
    }
    if (query.before) validateUuid(query.before, "cost event cursor");
    const params = new URLSearchParams({ limit: String(limit) });
    if (query.state) params.set("state", query.state);
    if (query.before) params.set("before", query.before);
    const envelope = await this.json<DataEnvelope<CostEventPage>>(
      `${PATHS.costEvents}?${params.toString()}`,
      undefined,
      { method: "GET" },
    );
    return envelope.data;
  }

  async costEvent(eventId: string): Promise<PendingCostEvent> {
    validateUuid(eventId, "cost event id");
    const envelope = await this.json<DataEnvelope<PendingCostEvent>>(
      `${PATHS.costEvents}/${eventId}`,
      undefined,
      { method: "GET" },
    );
    return envelope.data;
  }

  async replayCostEvent(eventId: string): Promise<PendingCostEvent> {
    validateUuid(eventId, "cost event id");
    const envelope = await this.json<DataEnvelope<PendingCostEvent>>(
      `${PATHS.costEvents}/${eventId}/replay`,
      {},
    );
    return envelope.data;
  }

  /** POST a binary-safe multipart form to a safe relative Relay endpoint. */
  async multipart<T = unknown>(path: string, form: MultipartForm): Promise<T> {
    const encoded = form.encode();
    const response = await this.requestEncoded(
      path,
      encoded.body,
      encoded.contentType,
    );
    if (!response.ok) throw await this.responseError(response);
    return this.decodeJson<T>(response);
  }

  /** Perform one AITP challenge/retry while replaying identical multipart bytes. */
  async paidMultipart<T = unknown>(
    path: string,
    form: MultipartForm,
    authorizer: AitpAuthorizer,
  ): Promise<T> {
    const encoded = form.encode();
    const response = await this.paidEncodedResponse(
      path,
      encoded.body,
      encoded.contentType,
      authorizer,
    );
    return this.decodeJson<T>(response);
  }

  /** Backward-compatible local list. Use catalog() for Relay's live snapshot. */
  models(): RelayModel[] {
    return this.knownModels.map((id) => ({
      id,
      object: "model" as const,
      kind: "model" as const,
      relay_target: relayModelTarget(id),
    }));
  }

  /**
   * Compatibility availability probe. `tensorzero::dryrun` prevents provider
   * execution; the result is not a numeric balance and may race later spends.
   */
  async credits(probeModel: string): Promise<CreditStatus> {
    if (!probeModel) throw new Error("credits() requires a probeModel");
    try {
      await this.chatCompletions({
        model: probeModel,
        messages: [{ role: "user", content: "ping" }],
        max_tokens: 1,
        "tensorzero::dryrun": true,
      });
      return { hasCredits: true, balance: null };
    } catch (error) {
      if (
        error instanceof InsufficientCreditsError ||
        error instanceof PaymentRequiredError
      ) {
        return { hasCredits: false, reason: error.message, balance: null };
      }
      throw error;
    }
  }

  async balance(probeModel: string): Promise<CreditStatus> {
    return this.credits(probeModel);
  }

  private async *openChatStream(
    params: ChatCompletionParams,
    authorizer?: AitpAuthorizer,
  ): AsyncGenerator<ChatCompletionChunk, void, unknown> {
    validateChatParams(params);
    const body = {
      ...params,
      stream: true,
      stream_options: {
        include_usage: true,
        ...(params.stream_options ?? {}),
      },
    };
    const response = authorizer
      ? await this.paidResponse(PATHS.chat, body, authorizer, "text/event-stream")
      : await this.request(PATHS.chat, body, { accept: "text/event-stream" });
    if (!response.ok) throw await this.responseError(response);
    if (!response.body) throw new Error("Relay streaming response had no body");

    const requestId = response.headers.get(AITP_REQUEST_ID_HEADER);
    let sawDone = false;
    try {
      yield* parseChatCompletionStream(response.body, {
        maxEventChars: this.maxSseEventChars,
        onDone: () => {
          sawDone = true;
        },
      });
    } finally {
      if (!sawDone && requestId && this.onAbandonedStream) {
        this.onAbandonedStream(requestId);
      }
    }
  }

  private async paidResponse(
    path: string,
    body: unknown,
    authorizer: AitpAuthorizer,
    accept = "application/json",
  ): Promise<FetchLikeResponse> {
    const first = await this.request(path, body, { accept });
    if (first.status !== 402) {
      if (!first.ok) throw await this.responseError(first);
      return first;
    }
    const error = await this.responseError(first);
    if (!(error instanceof PaymentRequiredError)) throw error;
    const requestId = error.payment.request_id;
    if (!requestId) throw new Error("AITP challenge is missing its request id");
    const authorization = validateRequiredHeader(
      await authorizer.authorize(error.payment),
      "AITP authorization",
    );
    const second = await this.request(path, body, {
      accept,
      headers: {
        [AITP_AUTHORIZATION_HEADER]: authorization,
        [AITP_REQUEST_ID_HEADER]: requestId,
      },
    });
    if (!second.ok) throw await this.responseError(second);
    return second;
  }

  private async paidEncodedResponse(
    path: string,
    body: Uint8Array,
    contentType: string,
    authorizer: AitpAuthorizer,
  ): Promise<FetchLikeResponse> {
    const first = await this.requestEncoded(path, body, contentType);
    if (first.status !== 402) {
      if (!first.ok) throw await this.responseError(first);
      return first;
    }
    const error = await this.responseError(first);
    if (!(error instanceof PaymentRequiredError)) throw error;
    const requestId = error.payment.request_id;
    if (!requestId) throw new Error("AITP challenge is missing its request id");
    const authorization = validateRequiredHeader(
      await authorizer.authorize(error.payment),
      "AITP authorization",
    );
    const second = await this.requestEncoded(path, body, contentType, {
      headers: {
        [AITP_AUTHORIZATION_HEADER]: authorization,
        [AITP_REQUEST_ID_HEADER]: requestId,
      },
    });
    if (!second.ok) throw await this.responseError(second);
    return second;
  }

  private async json<T>(
    path: string,
    body?: unknown,
    options: RequestOptions = {},
  ): Promise<T> {
    const response = await this.request(path, body, options);
    if (!response.ok && response.status !== options.acceptedErrorStatus) {
      throw await this.responseError(response);
    }
    return this.decodeJson<T>(response);
  }

  private async decodeJson<T>(response: FetchLikeResponse): Promise<T> {
    const text = await this.readBoundedText(response);
    try {
      return JSON.parse(text) as T;
    } catch {
      throw new Error("Relay returned malformed JSON");
    }
  }

  private async responseError(response: FetchLikeResponse) {
    let body: unknown;
    try {
      const text = await this.readBoundedText(response);
      body = text.length > 0 ? JSON.parse(text) : undefined;
    } catch (error) {
      body = error instanceof Error ? error.message : undefined;
    }
    const error = errorFromResponse(response.status, body, response.headers);
    if (!(error instanceof UnverifiedSettlementEvidenceError)) return error;
    if (!this.settlementEvidenceVerifier) return error;
    try {
      if (await this.settlementEvidenceVerifier(error.evidence)) {
        return new AlreadySettledError(error.evidence, error.body);
      }
    } catch {
      // Collapse verifier implementation failures into the same fail-closed
      // result as a bad signature; callers must never mistake either for proof.
    }
    return new InvalidSettlementEvidenceError(error.evidence, error.body);
  }

  private async readBoundedText(response: FetchLikeResponse): Promise<string> {
    const declared = response.headers.get("content-length");
    if (declared && Number(declared) > this.maxResponseBytes) {
      throw new Error(`Relay response exceeded ${this.maxResponseBytes} bytes`);
    }
    const text = await response.text();
    if (new TextEncoder().encode(text).byteLength > this.maxResponseBytes) {
      throw new Error(`Relay response exceeded ${this.maxResponseBytes} bytes`);
    }
    return text;
  }

  private async request(
    path: string,
    body?: unknown,
    options: RequestOptions = {},
  ): Promise<FetchLikeResponse> {
    const method = options.method ?? "POST";
    return this.requestEncoded(
      path,
      method === "GET" ? undefined : JSON.stringify(body ?? {}),
      method === "GET" ? undefined : "application/json",
      options,
    );
  }

  private async requestEncoded(
    path: string,
    body: string | Uint8Array | undefined,
    contentType?: string,
    options: RequestOptions = {},
  ): Promise<FetchLikeResponse> {
    if (!isSafeRelativePath(path)) throw new Error("Relay request path is unsafe");
    const method = options.method ?? "POST";
    const headers: Record<string, string> = {
      Accept: options.accept ?? "application/json",
      ...options.headers,
    };
    if (method !== "GET" && contentType) headers["Content-Type"] = contentType;
    if (this.apiKey) headers.Authorization = `Bearer ${this.apiKey}`;
    for (const [name, value] of Object.entries(headers)) {
      validateRequiredHeader(name, "header name");
      validateRequiredHeader(value, `header ${name}`);
    }

    const controller = this.timeoutMs != null ? new AbortController() : undefined;
    const timer =
      controller && this.timeoutMs != null
        ? setTimeout(() => controller.abort(), this.timeoutMs)
        : undefined;
    try {
      return await this.fetchImpl(`${this.baseUrl}/${path}`, {
        method,
        headers,
        body: method === "GET" ? undefined : body,
        signal: controller?.signal,
        redirect: "error",
      });
    } finally {
      if (timer) clearTimeout(timer);
    }
  }
}

export function relayModelTarget(model: string): string {
  if (model.startsWith("tensorzero::")) return model;
  return `tensorzero::model_name::${model}`;
}

export function relayFunctionTarget(fn: string): string {
  if (fn.startsWith("tensorzero::")) return fn;
  return `tensorzero::function_name::${fn}`;
}

function normalizeBaseUrl(raw: string): string {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    throw new Error("Relay baseUrl must be an absolute HTTP(S) URL");
  }
  if (!["http:", "https:"].includes(url.protocol) || !url.hostname) {
    throw new Error("Relay baseUrl must be an absolute HTTP(S) URL");
  }
  if (url.username || url.password || url.search || url.hash) {
    throw new Error("Relay baseUrl must not contain credentials, query, or fragment");
  }
  return url.toString().replace(/\/+$/, "");
}

function validateChatParams(params: ChatCompletionParams): void {
  if (!params || typeof params !== "object") {
    throw new Error("chatCompletions requires a params object");
  }
  if (typeof params.model !== "string" || params.model.length === 0) {
    throw new Error("chatCompletions requires a non-empty `model`");
  }
  if (!Array.isArray(params.messages) || params.messages.length === 0) {
    throw new Error("chatCompletions requires a non-empty `messages` array");
  }
  if (params.n != null && params.n !== 1) {
    throw new Error("Relay only supports `n` equal to 1 (or omitted)");
  }
}

function validateTokenCount(value: number, name: string): void {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${name} must be a non-negative safe integer`);
  }
}

function validateUuid(value: string, name: string): void {
  if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value)) {
    throw new Error(`${name} must be a UUID`);
  }
}

function validateOptionalPositiveInteger(
  value: number | undefined,
  name: string,
): number | undefined {
  if (value == null) return undefined;
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new Error(`${name} must be a positive safe integer`);
  }
  return value;
}

function validateOptionalHeader(
  value: string | undefined,
  name: string,
): string | undefined {
  return value == null ? undefined : validateRequiredHeader(value, name);
}

function validateRequiredHeader(value: string, name: string): string {
  if (!value.trim() || /[\r\n]/.test(value)) {
    throw new Error(`${name} must not be empty or contain newlines`);
  }
  return value;
}

function isSafeRelativePath(path: string): boolean {
  return (
    path.length > 0 &&
    !path.includes("://") &&
    !/[\r\n]/.test(path) &&
    !path.split(/[/?]/).includes("..")
  );
}
