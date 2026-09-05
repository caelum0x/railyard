-- Phase 14: integration connections (OAuth lifecycle) + resource mappings,
-- and usage-monitoring alerts + their trigger history.

CREATE TABLE IF NOT EXISTS integration_connections (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    integration_type varchar NOT NULL,       -- tax | accounting | crm
    provider_code varchar NOT NULL,          -- avalara | xero | salesforce | ...
    name varchar,
    access_token varchar,
    refresh_token varchar,
    token_expires_at timestamp(6),
    settings jsonb DEFAULT '{}' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_integration_connections_org
    ON integration_connections (organization_id, provider_code);

CREATE TABLE IF NOT EXISTS integration_mappings (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    integration_connection_id uuid NOT NULL,
    resource_type varchar NOT NULL,          -- invoice | customer
    lago_id uuid NOT NULL,
    external_id varchar NOT NULL,
    created_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_integration_mappings_resource
    ON integration_mappings (organization_id, resource_type, lago_id);

CREATE TABLE IF NOT EXISTS usage_alerts (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    subscription_external_id varchar NOT NULL,
    alert_type varchar NOT NULL,             -- current_usage_amount | billable_metric_current_usage_amount
    billable_metric_code varchar,            -- NULL = whole-subscription amount
    code varchar,
    threshold_cents bigint NOT NULL,
    recurring boolean DEFAULT false NOT NULL,
    previous_value_cents bigint DEFAULT 0 NOT NULL,
    last_triggered_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_usage_alerts_sub
    ON usage_alerts (organization_id, subscription_external_id);

CREATE TABLE IF NOT EXISTS usage_alert_triggers (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    usage_alert_id uuid NOT NULL,
    subscription_external_id varchar NOT NULL,
    crossed_threshold_cents bigint NOT NULL,
    current_value_cents bigint NOT NULL,
    triggered_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_usage_alert_triggers_alert
    ON usage_alert_triggers (usage_alert_id, triggered_at);
