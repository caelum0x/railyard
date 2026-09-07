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

export interface AitpChallenge {
  payee: string;
  amount_micros: number;
  currency: "AIT";
  resource: string;
  nonce: string;
  expiry: number;
  scheme: "aitp-hold";
}

export interface PaymentRequired {
  challenge: AitpChallenge;
  request_id?: string;
  protocol_version?: string;
}

export interface AitpAuthorizer {
  authorize(payment: PaymentRequired): string | Promise<string>;
}

/** Verify Mint's signed attestation using caller-pinned key material. */
export type SettlementEvidenceVerifier = (
  evidence: SettlementEvidence,
) => boolean | Promise<boolean>;

export interface GatewayStatus {
  status: string;
  version: string;
  config_hash: string;
}

export interface GatewayHealth {
  gateway: "ok" | "error";
  clickhouse: "ok" | "error";
  postgres: "ok" | "error";
  valkey: "ok" | "error";
  valkey_cache: "ok" | "error";
  relay_catalog?: "ok" | "error";
  relay_billing?: "ok" | "error" | "disabled";
}

export interface DataEnvelope<T> {
  data: T;
}

export type PricingSource =
  | "provider_list"
  | "contract"
  | "operator_override"
  | "conservative_fallback";

export interface ModelCapabilities {
  chat: boolean;
  embeddings: boolean;
  tools: boolean;
  vision: boolean;
  json_schema: boolean;
  streaming: boolean;
  batch: boolean;
}

export interface ModelPrice {
  input_micro_usd_per_million: number;
  output_micro_usd_per_million: number;
  source: PricingSource;
}

export interface CatalogModel {
  id: string;
  provider: string;
  display_name: string;
  aliases: string[];
  capabilities: ModelCapabilities;
  pricing: ModelPrice;
  context_window_tokens?: number;
  max_output_tokens?: number;
  enabled: boolean;
}

export interface CatalogSnapshot {
  schema_version: number;
  revision: string;
  generated_at_ms: number;
  models: CatalogModel[];
}

export interface PriceQuoteRequest {
  model: string;
  input_tokens: number;
  output_tokens: number;
}

export interface PriceQuote extends PriceQuoteRequest {
  amount_micros: number;
  pricing: ModelPrice;
  catalog_revision: string;
}

export interface SettlementReceiptProof {
  sequence: number;
  operation: string;
  operation_hash: string;
  amount_micros: number;
  balance_after_micros?: number;
  prev_hash?: string;
  receipt_hash: string;
  signature: string;
}

export interface SettlementEvidence {
  request_id: string;
  principal: string;
  receipt: SettlementReceiptProof;
  attestation?: unknown;
  verified_at_ms?: number;
  settled_at_ms: number;
}

export type InferenceRoute =
  | "unknown"
  | "native_inference"
  | "open_ai_chat_completions"
  | "open_ai_embeddings"
  | "batch_inference";

export interface CostEvent {
  schema_version: number;
  event_id: string;
  request_id: string;
  owner_principal: string;
  model: string;
  route: InferenceRoute;
  usage: {
    input_tokens: number;
    output_tokens: number;
    total_tokens: number;
    provider_cost_micros?: number;
  };
  authorized_micros: number;
  charged_micros: number;
  provider_cost_micros?: number;
  overage_capped: boolean;
  receipt_hash: string;
  settled_at_ms: number;
}

export interface PendingCostEvent {
  event: CostEvent;
  attempts: number;
  next_attempt_at_ms: number;
  lease_owner: string | null;
  lease_expires_at_ms: number | null;
  published_at_ms: number | null;
  dead_lettered_at_ms: number | null;
  last_error: string | null;
}

export type CostEventState = "pending" | "published" | "dead_lettered";

export interface CostEventListQuery {
  state?: CostEventState;
  before?: string;
  limit?: number;
}

export interface CostEventPage {
  items: PendingCostEvent[];
  next_cursor: string | null;
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
