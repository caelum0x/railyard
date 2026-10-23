// Row shapes of the `railyard` schema (db/migrations/0001_railyard_console.sql).

// "addon": the org is bound to a Scribase org and paid for by the Scribase
// portfolio add-on (Wave P PY1.0/PY1.1); Scribase owns its billing.
export type Plan = "none" | "trial" | "railyard" | "addon";
export type EntitlementStatus = "inactive" | "trialing" | "active" | "paused" | "cancelled";

export interface Org {
  id: string;
  name: string;
  owner_user_id: string;
  created_at: string;
  /** First-touch signup attribution slug (e.g. "insights"), or null (db/migrations/0011). */
  signup_source: string | null;
}

export interface App {
  id: string;
  org_id: string;
  name: string;
  site_key: string;
  created_at: string;
}

export interface Entitlement {
  org_id: string;
  plan: Plan;
  status: EntitlementStatus;
  interval: "month" | "year" | null;
  apps_limit: number;
  events_limit: number;
  credit_cents: number;
  card_verified_at: string | null;
  card_check_payment_id: string | null;
  trial_ends_at: string | null;
  dodo_customer_id: string | null;
  subscription_id: string | null;
  paused: boolean;
  cancel_at_period_end: boolean;
  updated_at: string;
  /** Add-on orgs only (migration 0002): the slots the Scribase add-on unlocks. */
  addon_slots?: string[];
  addon_renews_at?: string | null;
  /**
   * Add-on orgs only (migration 0004): the Scribase time (epoch ms) of the
   * add-on state this row mirrors. A bridge sync older than this is ignored,
   * so a late delivery can never flip the entitlement back (P-Y1).
   */
  addon_as_of?: number | null;
}

/** A Railyard org created by the Scribase bridge, bound to one Scribase org (migration 0002). */
export interface ScribaseBinding {
  scribase_org_id: string;
  org_id: string;
  created_at: string;
}

export interface BillingEvent {
  webhook_id: string;
  org_id: string | null;
  event_type: string;
  payment_id: string | null;
  kind: string | null;
  amount_cents: number | null;
  outcome: string;
  received_at?: string;
}

export interface Deploy {
  id: string;
  org_id: string;
  app_id: string;
  url: string;
  commit_sha: string | null;
  branch: string;
  framework: "nextjs" | "vite" | "astro" | "static";
  version_id: string | null;
  duration_ms: number | null;
  status: "running" | "succeeded" | "failed" | "rolled_back";
  rollback_of: string | null;
  created_at: string;
}

export interface ApiToken {
  id: string;
  org_id: string;
  user_id: string;
  token_hash: string;
  label: string;
  created_at: string;
  last_used_at: string | null;
  revoked_at: string | null;
}

export interface DeviceCode {
  device_code_hash: string;
  user_code: string;
  org_id: string | null;
  user_id: string | null;
  approved_at: string | null;
  consumed_at: string | null;
  expires_at: string;
}

export function emptyEntitlement(orgId: string, now: Date): Entitlement {
  return {
    org_id: orgId,
    plan: "none",
    status: "inactive",
    interval: null,
    apps_limit: 0,
    events_limit: 0,
    credit_cents: 0,
    card_verified_at: null,
    card_check_payment_id: null,
    trial_ends_at: null,
    dodo_customer_id: null,
    subscription_id: null,
    paused: false,
    cancel_at_period_end: false,
    updated_at: now.toISOString(),
  };
}

// AI gateway (Wave P PY1.4, migration 0003). BYOK only.
export type AiProvider = "openai" | "anthropic" | "groq" | "openrouter";

export interface AiKey {
  id: string;
  org_id: string;
  label: string;
  key_hash: string;
  daily_token_cap: number;
  monthly_token_cap: number;
  created_at: string;
  revoked_at: string | null;
}

export interface AiCredential {
  org_id: string;
  provider: AiProvider;
  nonce: string;
  ciphertext: string;
  key_hint: string;
  updated_at: string;
}

export interface AiTrace {
  key_id: string;
  at: string;
  model: string;
  provider: string;
  status: number;
  input_tokens: number;
  output_tokens: number;
  latency_ms: number;
}

export interface ConnectToken {
  token_hash: string;
  org_id: string;
  purpose: "ai";
  expires_at: string;
  used_at: string | null;
}
