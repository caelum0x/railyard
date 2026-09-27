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
  /** Cursor for the next (older) page; absent on the last page. */
  next_before?: string;
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

/** Aggregation scope for billing reads. `all` requires an operator key. */
export type BillingScope = "own" | "all";

/** Mirrors `relay_access::CostFeedStatus`. */
export interface CostFeedStatus {
  configured: boolean;
  format?: "raw" | "rago";
  target_host?: string;
  billable_metric_code?: string;
}

/** Mirrors `relay_access::RelayBillingStatusResponse` (`GET /relay/billing/status`). */
export interface BillingStatus {
  billing_enabled: boolean;
  metering_enabled: boolean;
  auth_enabled: boolean;
  postgres_configured: boolean;
  budgets_enforced: boolean;
  cost_feed: CostFeedStatus;
  principal?: string;
  operator: boolean;
  operator_keys_configured: boolean;
  warnings: string[];
}

export type CostEventState = "pending" | "published" | "dead_lettered";

export type InferenceRoute =
  | "unknown"
  | "native_inference"
  | "open_ai_chat_completions"
  | "open_ai_embeddings"
  | "batch_inference";

/** Mirrors `relay_cost_feed::CostEvent`. */
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
  /** `metered:<request_id>` for postpaid meter-only events. */
  receipt_hash: string;
  settled_at_ms: number;
}

/** Mirrors `relay_cost_feed::PendingCostEvent`. */
export interface CostEventRecord {
  event: CostEvent;
  attempts: number;
  next_attempt_at_ms: number;
  lease_owner: string | null;
  lease_expires_at_ms: number | null;
  published_at_ms: number | null;
  dead_lettered_at_ms: number | null;
  last_error: string | null;
}

/** Mirrors `relay_cost_feed::CostEventPage`. */
export interface CostEventPage {
  items: CostEventRecord[];
  next_cursor: string | null;
}

/** Mirrors `relay_storage_postgres::KeyBudgetStatus`. */
export interface KeyBudget {
  owner_principal: string;
  monthly_limit_micros: number;
  month_to_date_micros: number;
  remaining_micros: number;
  /** Optional per-UTC-day cap. */
  daily_limit_micros?: number;
  day_to_date_micros?: number;
  exceeded: boolean;
  /** The daily cap is configured and reached (blocked until the next UTC day). */
  daily_exceeded?: boolean;
  note?: string;
  updated_by: string;
  updated_at_ms: number;
}

export function costEventState(record: CostEventRecord): CostEventState {
  if (record.dead_lettered_at_ms != null) {
    return "dead_lettered";
  }
  if (record.published_at_ms != null) {
    return "published";
  }
  return "pending";
}

export function isMeteredEvent(event: CostEvent): boolean {
  return event.receipt_hash.startsWith("metered:");
}

/** Mirrors `relay_access::WalletBalanceResponse` (`GET /relay/billing/wallet`). */
export interface WalletBalance {
  principal: string;
  billing_enabled: boolean;
  /** Mint account paying for this key's inference (prepaid mode only). */
  payer_account?: string;
  available_micros?: number;
  /** Checkout providers offered for self-serve top-ups (empty when none). */
  top_up_providers: string[];
  min_top_up_micros: number;
  max_top_up_micros: number;
  /** Whether the caller may grant operator credit from the treasury. */
  operator_credit_available: boolean;
}

/** Mirrors `relay_billing::TopUpCheckout` (`POST /relay/billing/wallet/top_up`). */
export interface TopUpCheckout {
  provider: string;
  checkout_id: string;
  checkout_url: string;
  account_id: string;
  amount_micros: number;
}

/** Mirrors `relay_billing::AccountCredit` (`POST /relay/billing/wallet/credit`). */
export interface WalletCredit {
  from_account: string;
  to_account: string;
  amount_micros: number;
  idempotent_replay: boolean;
}
