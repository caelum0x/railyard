-- Activity + audit logs (Lago writes these to ClickHouse; here in Postgres).
CREATE TABLE IF NOT EXISTS activity_logs (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    activity_type varchar NOT NULL,
    activity_source varchar DEFAULT 'system' NOT NULL,
    resource_type varchar,
    resource_id uuid,
    external_customer_id varchar,
    external_subscription_id varchar,
    payload jsonb DEFAULT '{}' NOT NULL,
    created_at timestamp(6) DEFAULT now() NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_activity_logs_org ON activity_logs (organization_id, created_at);

CREATE TABLE IF NOT EXISTS audit_logs (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    resource_type varchar NOT NULL,
    resource_id uuid NOT NULL,
    action varchar NOT NULL,
    changes jsonb DEFAULT '{}' NOT NULL,
    created_at timestamp(6) DEFAULT now() NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_audit_logs_org ON audit_logs (organization_id, created_at);
