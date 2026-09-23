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
