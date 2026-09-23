/**
 * Billing summary transport types for the Relay billing dashboard.
 *
 * These mirror the `relay_contract::BillingSummary` types returned by the
 * gateway `GET /relay/billing/summary` endpoint. All micro-USD amounts are
 * exact integers (micro-dollars: 1_000_000 == $1).
 */

export interface WalletSummary {
  total_charged_micros: number;
  total_authorized_micros: number;
  total_settled_micros: number;
  cost_event_count: number;
  settlement_count: number;
  last_settled_at_ms?: number;
}

export interface SpendTotals {
  charged_micros: number;
  authorized_micros: number;
  input_tokens: number;
  output_tokens: number;
  total_tokens: number;
}

export interface KeySpend {
  key: string;
  charged_micros: number;
  cost_event_count: number;
}

export interface ModelSpend {
  model: string;
  charged_micros: number;
  total_tokens: number;
  cost_event_count: number;
}

export interface SettlementRecord {
  request_id: string;
  amount_micros: number;
  settled_at_ms: number;
  verified_at_ms?: number;
}

export interface BillingSummary {
  principal: string;
  wallet: WalletSummary;
  totals: SpendTotals;
  by_key: KeySpend[];
  by_model: ModelSpend[];
  recent_settlements: SettlementRecord[];
}

/**
 * One settlement receipt with full evidence detail. Mirrors
 * `relay_contract::SettlementDetail` returned by `GET /relay/billing/settlements`.
 */
export interface SettlementDetail {
  request_id: string;
  amount_micros: number;
  settled_at_ms: number;
  verified_at_ms?: number;
  has_attestation: boolean;
  /** The immutable signed settlement receipt, serialized as a JSON string. */
  receipt_json: string;
}

/** Mirrors `relay_contract::SettlementHistory`. */
export interface SettlementHistory {
  principal: string;
  settlements: SettlementDetail[];
}

/**
 * One `(key, day-bucket)` spend point. Mirrors `relay_contract::KeySpendPoint`
 * returned by `GET /relay/billing/keys`.
 */
export interface KeySpendPoint {
  key: string;
  bucket_start_ms: number;
  charged_micros: number;
  total_tokens: number;
  cost_event_count: number;
}

/** Mirrors `relay_contract::KeySpendSeries`. */
export interface KeySpendSeries {
  principal: string;
  points: KeySpendPoint[];
}
