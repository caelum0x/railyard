/**
 * Per-API-key trace transport types (RY-RL3).
 *
 * These mirror the `relay_traces` crate types returned by the gateway
 * `GET /relay/traces` and `GET /relay/traces/{inference_id}` endpoints.
 * Optional fields are omitted by the gateway when unknown. Costs are exact
 * micro-USD integers (1_000_000 == $1). Times are microseconds since the epoch.
 */

export interface TraceSummary {
  inference_id: string;
  episode_id: string;
  kind: "chat" | "json";
  function_name: string;
  variant_name: string;
  api_key_public_id?: string;
  created_at_us: number;
  model_name?: string;
  model_provider_name?: string;
  finish_reason?: string;
  input_tokens?: number;
  output_tokens?: number;
  cost_micros?: number;
  processing_time_ms?: number;
  ttft_ms?: number;
  model_calls: number;
  cached: boolean;
}

export interface TracePage {
  traces: TraceSummary[];
  next_before?: string;
  window_hours: number;
  key?: string;
}

export interface TraceModelCall {
  id: string;
  model_name: string;
  model_provider_name: string;
  input_tokens?: number;
  output_tokens?: number;
  response_time_ms?: number;
  ttft_ms?: number;
  cost_micros?: number;
  cached: boolean;
  finish_reason?: string;
  created_at_us: number;
}

export interface TraceDetail {
  summary: TraceSummary;
  model_calls: TraceModelCall[];
  input?: unknown;
  output?: unknown;
  bodies_expired: boolean;
}

export type TraceScope = "own" | "all";

export interface TraceListOptions {
  scope?: TraceScope;
  key?: string;
  limit?: number;
  windowHours?: number;
  before?: string;
  functionName?: string;
}

/** Build the gateway query string for a trace list request. */
export function traceListQuery(options: TraceListOptions = {}): string {
  const params = new URLSearchParams();
  if (options.scope === "all") {
    params.set("scope", "all");
    if (options.key) {
      params.set("key", options.key);
    }
  }
  if (options.limit != null) {
    params.set("limit", String(options.limit));
  }
  if (options.windowHours != null) {
    params.set("window_hours", String(options.windowHours));
  }
  if (options.before) {
    params.set("before", options.before);
  }
  if (options.functionName) {
    params.set("function", options.functionName);
  }
  const query = params.toString();
  return query ? `?${query}` : "";
}
