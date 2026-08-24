/**
 * Wire types for the Relay gateway's OpenAI-compatible surface.
 *
 * These mirror `OpenAICompatibleParams` / `OpenAICompatibleResponse` from the
 * gateway (apps/relay/crates/tensorzero-core/src/endpoints/openai_compatible).
 * Relay-specific extensions use the `tensorzero::*` key convention and are
 * passed through verbatim.
 */

export type ChatRole = "system" | "user" | "assistant" | "tool";

export interface ChatMessage {
  role: ChatRole;
  content: string | null;
  /** Present on assistant messages that requested tool calls. */
  tool_calls?: ToolCall[];
  /** Required when role is "tool"; ties the result back to a tool call. */
  tool_call_id?: string;
  name?: string;
}

export interface ToolCall {
  id: string;
  type: "function";
  function: {
    name: string;
    arguments: string;
  };
}

/** Passthrough Relay-native params keyed under the `tensorzero::` namespace. */
export interface RelayExtensions {
  "tensorzero::episode_id"?: string;
  "tensorzero::include_raw_usage"?: boolean;
  "tensorzero::variant_name"?: string;
  // Any other tensorzero::* keys are accepted verbatim.
  [key: `tensorzero::${string}`]: unknown;
}

export interface StreamOptions {
  include_usage?: boolean;
}

/**
 * Request body for POST /openai/v1/chat/completions.
 *
 * `model` is REQUIRED. Relay targets are addressed as
 * `tensorzero::model_name::<model>` or `tensorzero::function_name::<function>`.
 */
export interface ChatCompletionParams extends RelayExtensions {
  model: string;
  messages: ChatMessage[];
  max_tokens?: number;
  temperature?: number;
  top_p?: number;
  /** Relay requires n to be 1 or omitted. */
  n?: number;
  stop?: string | string[];
  stream?: boolean;
  stream_options?: StreamOptions;
}

export type FinishReason =
  | "stop"
  | "length"
  | "content_filter"
  | "tool_calls"
  | null;

export interface Usage {
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
}

export interface ChatCompletionChoice {
  index: number;
  finish_reason: FinishReason;
  message: ChatMessage;
}

/** Non-streaming response from POST /openai/v1/chat/completions. */
export interface ChatCompletion {
  id: string;
  episode_id?: string;
  object: string;
  created: number;
  model: string;
  choices: ChatCompletionChoice[];
  usage: Usage;
  system_fingerprint?: string;
  service_tier?: string;
  tensorzero_raw_usage?: unknown;
  tensorzero_raw_response?: unknown;
}

export interface ChatCompletionDelta {
  role?: ChatRole;
  content?: string | null;
  tool_calls?: ToolCall[];
}

export interface ChatCompletionChunkChoice {
  index: number;
  finish_reason: FinishReason;
  delta: ChatCompletionDelta;
}

/** A single SSE frame from a streamed chat completion. */
export interface ChatCompletionChunk {
  id: string;
  episode_id?: string;
  object: string;
  created: number;
  model: string;
  choices: ChatCompletionChunkChoice[];
  /** Only present in the terminal chunk when include_usage is set. */
  usage?: Usage;
}

/** A model entry surfaced by RelayClient.models(). */
export interface RelayModel {
  id: string;
  object: "model";
  /**
   * How to address this target on the chat completions API, e.g.
   * `tensorzero::model_name::gpt-4o-mini`.
   */
  relay_target: string;
  kind: "model" | "function";
}

/**
 * Result of RelayClient.credits().
 *
 * The gateway exposes no balance endpoint; billing is enforced by middleware
 * that returns 402 `insufficient_credits`. This SDK therefore reports whether
 * inference is currently permitted rather than inventing a numeric balance.
 */
export interface CreditStatus {
  /** True when a probe inference was NOT rejected with 402. */
  hasCredits: boolean;
  /** Set when billing rejected the probe with a typed reason. */
  reason?: string;
  /**
   * Balance is unknown: the gateway has no /credits or /balance route.
   * Always null until a first-class endpoint exists.
   */
  balance: number | null;
}
