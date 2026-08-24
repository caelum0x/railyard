/**
 * RelayClient — native client for the Relay inference gateway.
 *
 * Wraps the gateway's OpenAI-compatible HTTP surface without depending on the
 * `openai` package. The base URL is the gateway root; chat calls are issued to
 * `${baseUrl}/openai/v1/chat/completions`. Every request carries
 * `Authorization: Bearer <apiKey>` and `Content-Type: application/json`.
 */

import { errorFromResponse, InsufficientCreditsError } from "./errors.js";
import { parseChatCompletionStream } from "./stream.js";
import type {
  ChatCompletion,
  ChatCompletionChunk,
  ChatCompletionParams,
  CreditStatus,
  RelayModel,
} from "./types.js";

/** Minimal fetch signature so tests can inject a mock without DOM lib types. */
export type FetchLike = (
  input: string,
  init?: {
    method?: string;
    headers?: Record<string, string>;
    body?: string;
    signal?: AbortSignal;
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
  /** Gateway root, e.g. "https://gateway.relay.example" or "http://localhost:3000". */
  baseUrl: string;
  /** Bearer key sent on every request (when gateway auth is enabled). */
  apiKey?: string;
  /** Inject a custom fetch (defaults to global fetch). Useful for tests. */
  fetch?: FetchLike;
  /**
   * Known model/function targets for models(). The gateway exposes no public
   * list route, so callers supply the catalog they care about.
   */
  knownModels?: string[];
  /** Default request timeout in ms. */
  timeoutMs?: number;
}

const CHAT_PATH = "/openai/v1/chat/completions";

export class RelayClient {
  private readonly baseUrl: string;
  private readonly apiKey?: string;
  private readonly fetchImpl: FetchLike;
  private readonly knownModels: string[];
  private readonly timeoutMs?: number;

  constructor(options: RelayClientOptions) {
    if (!options || typeof options.baseUrl !== "string" || options.baseUrl.length === 0) {
      throw new Error("RelayClient requires a non-empty baseUrl");
    }
    // Strip a single trailing slash so path joins are predictable.
    this.baseUrl = options.baseUrl.replace(/\/+$/, "");
    this.apiKey = options.apiKey;
    this.knownModels = options.knownModels ?? [];
    this.timeoutMs = options.timeoutMs;

    const injected = options.fetch;
    if (injected) {
      this.fetchImpl = injected;
    } else if (typeof globalThis.fetch === "function") {
      // Bind to avoid "Illegal invocation" in some runtimes.
      this.fetchImpl = globalThis.fetch.bind(globalThis) as unknown as FetchLike;
    } else {
      throw new Error(
        "No fetch implementation available. Pass options.fetch or run on Node >= 18.",
      );
    }
  }

  /**
   * POST /openai/v1/chat/completions with stream:false. Returns the parsed
   * OpenAI-compatible response.
   */
  async chatCompletions(
    params: ChatCompletionParams,
  ): Promise<ChatCompletion> {
    validateChatParams(params);
    const body = { ...params, stream: false };
    const res = await this.request(CHAT_PATH, body);
    if (!res.ok) {
      throw errorFromResponse(res.status, await safeJson(res));
    }
    return (await res.json()) as ChatCompletion;
  }

  /**
   * POST /openai/v1/chat/completions with stream:true. Returns an async
   * iterator over typed SSE chunks. Sets stream_options.include_usage so the
   * terminal chunk carries a usage object (unless the caller overrode it).
   */
  async *chatCompletionsStream(
    params: ChatCompletionParams,
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

    const res = await this.request(CHAT_PATH, body, { accept: "text/event-stream" });
    if (!res.ok) {
      throw errorFromResponse(res.status, await safeJson(res));
    }
    if (!res.body) {
      throw new Error("Relay streaming response had no body");
    }
    yield* parseChatCompletionStream(res.body);
  }

  /**
   * List available model/function targets.
   *
   * The Relay gateway has NO public list-models route, so this returns the
   * catalog supplied via `knownModels` (defaulting to empty), annotated with
   * the `tensorzero::model_name::*` address each target uses on the chat API.
   * It does NOT fabricate an OpenAI /v1/models payload from the network.
   */
  models(): RelayModel[] {
    return this.knownModels.map((id) => ({
      id,
      object: "model" as const,
      kind: "model" as const,
      relay_target: relayModelTarget(id),
    }));
  }

  /**
   * Read credit availability.
   *
   * The gateway exposes no /credits or /balance route; billing is enforced by
   * middleware that returns 402 `insufficient_credits`. This helper issues a
   * minimal probe inference and normalizes the outcome: a 402 means no credits,
   * any other outcome means inference is currently permitted. The numeric
   * balance is unknown and always reported as null.
   *
   * @param probeModel model target to probe with (required — Relay demands a model).
   */
  async credits(probeModel: string): Promise<CreditStatus> {
    if (!probeModel) {
      throw new Error("credits() requires a probeModel to send a billing probe");
    }
    try {
      await this.chatCompletions({
        model: probeModel,
        messages: [{ role: "user", content: "ping" }],
        max_tokens: 1,
      });
      return { hasCredits: true, balance: null };
    } catch (error) {
      if (error instanceof InsufficientCreditsError) {
        return { hasCredits: false, reason: error.message, balance: null };
      }
      throw error;
    }
  }

  /** Alias for credits(); balance figures are not exposed by the gateway. */
  async balance(probeModel: string): Promise<CreditStatus> {
    return this.credits(probeModel);
  }

  private async request(
    path: string,
    body: unknown,
    opts: { accept?: string } = {},
  ): Promise<FetchLikeResponse> {
    const headers: Record<string, string> = {
      "Content-Type": "application/json",
      Accept: opts.accept ?? "application/json",
    };
    if (this.apiKey) {
      headers.Authorization = `Bearer ${this.apiKey}`;
    }

    const controller =
      this.timeoutMs != null ? new AbortController() : undefined;
    const timer =
      controller != null && this.timeoutMs != null
        ? setTimeout(() => controller.abort(), this.timeoutMs)
        : undefined;

    try {
      return await this.fetchImpl(`${this.baseUrl}${path}`, {
        method: "POST",
        headers,
        body: JSON.stringify(body),
        signal: controller?.signal,
      });
    } finally {
      if (timer) clearTimeout(timer);
    }
  }
}

/** Build the Relay chat-API address for a bare model id. */
export function relayModelTarget(model: string): string {
  if (model.startsWith("tensorzero::")) return model;
  return `tensorzero::model_name::${model}`;
}

/** Build the Relay chat-API address for a Relay function. */
export function relayFunctionTarget(fn: string): string {
  if (fn.startsWith("tensorzero::")) return fn;
  return `tensorzero::function_name::${fn}`;
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

async function safeJson(res: FetchLikeResponse): Promise<unknown> {
  try {
    return await res.json();
  } catch {
    try {
      return await res.text();
    } catch {
      return undefined;
    }
  }
}
