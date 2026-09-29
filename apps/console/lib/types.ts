// Row shapes of the `railyard` schema (db/migrations/0001_railyard_console.sql).

export type Plan = "none" | "trial" | "railyard";
export type EntitlementStatus = "inactive" | "trialing" | "active" | "paused" | "cancelled";

export interface Org {
  id: string;
  name: string;
  owner_user_id: string;
  created_at: string;
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
