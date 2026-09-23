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
