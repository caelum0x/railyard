-- GraphQL admin CRUD, slice 2: the remaining catalog/billing tables the
-- dashboard manages. Column names/types follow Lago's db/structure.sql so the
-- port stays wire-compatible with a real Lago DB.

-- Per-item credit-note lines (the amounts credited against individual fees).
CREATE TABLE IF NOT EXISTS credit_note_items (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    credit_note_id uuid,
    fee_id uuid NOT NULL,
    amount_cents bigint DEFAULT 0 NOT NULL,
    precise_amount_cents numeric(30,15) DEFAULT 0.0 NOT NULL,
    amount_currency varchar NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS index_credit_note_items_on_credit_note_id ON credit_note_items (credit_note_id);

-- Usage-monitoring alerts (unified across subscriptions + wallets) and their
-- thresholds (Lago `alerts` + `usage_monitoring_thresholds`).
CREATE TABLE IF NOT EXISTS alerts (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    alertable_type varchar NOT NULL,          -- 'Subscription' | 'Wallet'
    alertable_id uuid NOT NULL,
    alert_type varchar NOT NULL,
    name varchar,
    code varchar NOT NULL,
    billable_metric_id uuid,
    previous_value numeric(30,5) DEFAULT 0.0 NOT NULL,
    last_processed_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);
CREATE INDEX IF NOT EXISTS index_alerts_on_alertable ON alerts (alertable_type, alertable_id);

CREATE TABLE IF NOT EXISTS alert_thresholds (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    alert_id uuid NOT NULL,
    code varchar,
    value numeric(30,5) NOT NULL,
    recurring boolean DEFAULT false NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS index_alert_thresholds_on_alert_id ON alert_thresholds (alert_id);

-- Billing entities (multi-entity invoicing under one organization).
CREATE TABLE IF NOT EXISTS billing_entities (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    code varchar NOT NULL,
    name varchar NOT NULL,
    email varchar,
    country varchar,
    default_currency varchar DEFAULT 'USD' NOT NULL,
    timezone varchar DEFAULT 'UTC' NOT NULL,
    applied_dunning_campaign_id uuid,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

-- Invoice custom sections (reusable invoice header/footer content).
CREATE TABLE IF NOT EXISTS invoice_custom_sections (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    code varchar NOT NULL,
    name varchar NOT NULL,
    description varchar,
    details text,
    display_name varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

-- Adjusted fees (manual overrides of a draft-invoice fee's units/amount).
CREATE TABLE IF NOT EXISTS adjusted_fees (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    fee_id uuid,
    invoice_id uuid NOT NULL,
    charge_id uuid,
    subscription_id uuid,
    properties jsonb DEFAULT '{}' NOT NULL,
    adjusted_units boolean DEFAULT false NOT NULL,
    adjusted_amount boolean DEFAULT false NOT NULL,
    units numeric DEFAULT 0.0 NOT NULL,
    unit_amount_cents bigint DEFAULT 0 NOT NULL,
    invoice_display_name varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

-- Fixed charges (recurring flat charges attached to a plan).
CREATE TABLE IF NOT EXISTS fixed_charges (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    plan_id uuid NOT NULL,
    add_on_id uuid,
    charge_model integer DEFAULT 0 NOT NULL,
    properties jsonb DEFAULT '{}' NOT NULL,
    units numeric DEFAULT 0.0 NOT NULL,
    pay_in_advance boolean DEFAULT false NOT NULL,
    prorated boolean DEFAULT false NOT NULL,
    invoice_display_name varchar,
    parent_id uuid,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);
CREATE INDEX IF NOT EXISTS index_fixed_charges_on_plan_id ON fixed_charges (plan_id);
