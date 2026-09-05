-- Phase 15: dunning-campaign configuration + lifetime-usage accumulation.

CREATE TABLE IF NOT EXISTS dunning_campaigns (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    name varchar NOT NULL,
    code varchar NOT NULL,
    description varchar,
    max_attempts integer DEFAULT 3 NOT NULL,
    days_between_attempts integer DEFAULT 7 NOT NULL,
    bcc_emails jsonb DEFAULT '[]' NOT NULL,
    applied_to_organization boolean DEFAULT false NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_dunning_campaigns_org ON dunning_campaigns (organization_id);

-- Cumulative (lifetime) usage per subscription — the invoiced total plus the
-- live current-period usage, used for lifetime-usage threshold alerts and
-- progressive-billing true-ups.
CREATE TABLE IF NOT EXISTS lifetime_usages (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    subscription_external_id varchar NOT NULL,
    invoiced_usage_amount_cents bigint DEFAULT 0 NOT NULL,
    current_usage_amount_cents bigint DEFAULT 0 NOT NULL,
    updated_at timestamp(6) NOT NULL,
    UNIQUE (organization_id, subscription_external_id)
);
