/**
 * Relay model catalog transport types for the model catalog page.
 *
 * These mirror the `relay_catalog::CatalogSnapshot` types returned by the
 * gateway `GET /v1/relay/catalog` endpoint.
 */

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

/** Mirrors `relay_catalog::PriceQuote` returned by `POST /v1/relay/quotes`. */
export interface PriceQuote {
  model: string;
  input_tokens: number;
  output_tokens: number;
  amount_micros: number;
  pricing: ModelPrice;
  catalog_revision: string;
}

/** Mirrors `provider_status::ProviderRouteStatus`. */
export interface ProviderRouteStatus {
  provider: string;
  priority: number;
  circuit: "closed" | "open" | "half_open";
  circuit_cooldown_ms?: number;
  ewma_latency_ms: number;
  ewma_ttft_ms: number;
  error_rate: number;
  total_attempts: number;
  failed_attempts: number;
  observed: boolean;
}

/** Mirrors `provider_status::ModelRoutingStatus`. */
export interface ModelRoutingStatus {
  model: string;
  hedging_enabled: boolean;
  hedge_max_concurrency: number;
  has_fallback: boolean;
  providers: ProviderRouteStatus[];
}

/** Mirrors `provider_status::ProviderHealthReport`. */
export interface ProviderHealthReport {
  generated_at_ms: number;
  models: ModelRoutingStatus[];
  unrouted: ProviderRouteStatus[];
}

/** One persisted operator price edit (`GET /v1/relay/catalog/prices`). */
export interface PriceEdit {
  model_id: string;
  input_micro_usd_per_million: number;
  output_micro_usd_per_million: number;
  /** The deployed catalog price this edit replaces. */
  deployed_input_micro_usd_per_million?: number | null;
  deployed_output_micro_usd_per_million?: number | null;
  note?: string;
  updated_by: string;
  updated_at_ms: number;
}

/** Mirrors `catalog_admin::PriceEditorState`. */
export interface PriceEditorState {
  storage_available: boolean;
  operator: boolean;
  unavailable_reason?: string;
  deployed_revision: string;
  edits: PriceEdit[];
}

export type ProviderKeySource = "stored" | "environment" | "missing";

export interface ProviderKeyTest {
  ok: boolean;
  message: string;
  tested_at_ms: number;
}

/** Mirrors `provider_keys::ProviderKeyView`. Never carries the secret. */
export interface ProviderKey {
  name: string;
  provider: string;
  label: string;
  env_var: string;
  source: ProviderKeySource;
  key_hint?: string;
  stored: boolean;
  updated_by?: string;
  updated_at_ms?: number;
  last_test?: ProviderKeyTest;
  error?: string;
}

/** Mirrors `provider_keys::ProviderKeysResponse`. */
export interface ProviderKeysState {
  management_available: boolean;
  unavailable_reason?: string;
  operator: boolean;
  keys: ProviderKey[];
}

/** Mirrors `provider_keys::ProviderKeyTestResult`. */
export interface ProviderKeyTestResult {
  name: string;
  source: ProviderKeySource;
  ok: boolean;
  message: string;
}
