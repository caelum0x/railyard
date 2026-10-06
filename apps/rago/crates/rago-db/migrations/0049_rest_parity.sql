-- REST parity with Lago's shared API (round 2).
--
-- Metadata on plans, wallets and credit notes (the invoice column exists).
ALTER TABLE plans ADD COLUMN IF NOT EXISTS metadata jsonb DEFAULT '{}' NOT NULL;
ALTER TABLE wallets ADD COLUMN IF NOT EXISTS metadata jsonb DEFAULT '{}' NOT NULL;
ALTER TABLE credit_notes ADD COLUMN IF NOT EXISTS metadata jsonb DEFAULT '{}' NOT NULL;

-- A customer's wallets are addressed by code.
CREATE INDEX IF NOT EXISTS idx_wallets_customer_code ON wallets (customer_id, code) WHERE code IS NOT NULL;

-- A plan feature (or one of its privileges) removed for one subscription.
CREATE TABLE IF NOT EXISTS entitlement_subscription_feature_removals (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    subscription_id uuid NOT NULL,
    entitlement_feature_id uuid NOT NULL,
    entitlement_privilege_id uuid,
    created_at timestamp(6) DEFAULT now() NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_feature_removals_unique
    ON entitlement_subscription_feature_removals (subscription_id, entitlement_feature_id, COALESCE(entitlement_privilege_id, '00000000-0000-0000-0000-000000000000'::uuid));

-- Lifetime usage carried in from before Rago (Lago external_historical_usage_amount_cents).
ALTER TABLE lifetime_usages ADD COLUMN IF NOT EXISTS historical_usage_amount_cents bigint DEFAULT 0 NOT NULL;

-- Every mutating API request (Lago api_logs).
CREATE TABLE IF NOT EXISTS api_logs (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    request_id uuid NOT NULL UNIQUE,
    api_key_id uuid,
    api_version varchar NOT NULL,
    http_method varchar NOT NULL,
    http_status integer NOT NULL,
    request_path varchar NOT NULL,
    request_origin varchar,
    client varchar,
    request_body jsonb,
    created_at timestamp(6) DEFAULT now() NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_api_logs_org_created ON api_logs (organization_id, created_at DESC);

-- Security-relevant changes: API keys, webhook endpoints, members, sign-ins (Lago security_logs).
CREATE TABLE IF NOT EXISTS security_logs (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    log_type varchar NOT NULL,
    log_event varchar NOT NULL,
    user_id uuid,
    api_key_id uuid,
    resources jsonb DEFAULT '{}' NOT NULL,
    device_info jsonb DEFAULT '{}' NOT NULL,
    created_at timestamp(6) DEFAULT now() NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_security_logs_org_created ON security_logs (organization_id, created_at DESC);
