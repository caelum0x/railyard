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
