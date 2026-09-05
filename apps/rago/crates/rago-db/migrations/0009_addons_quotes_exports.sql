-- Add-ons, quotes, data exports, wallet recurring top-up rules.

CREATE TABLE IF NOT EXISTS add_ons (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    name varchar NOT NULL,
    code varchar NOT NULL,
    description varchar,
    amount_cents bigint NOT NULL,
    amount_currency varchar NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS quotes (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    customer_id uuid NOT NULL,
    number varchar NOT NULL,
    version_number integer DEFAULT 1 NOT NULL,
    status varchar DEFAULT 'draft' NOT NULL, -- draft | accepted | expired
    currency varchar NOT NULL,
    sub_total_amount_cents bigint DEFAULT 0 NOT NULL,
    taxes_amount_cents bigint DEFAULT 0 NOT NULL,
    total_amount_cents bigint DEFAULT 0 NOT NULL,
    line_items jsonb DEFAULT '[]' NOT NULL,
    accepted_invoice_id uuid,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS data_exports (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    resource_type varchar NOT NULL, -- invoices | events | customers
    format varchar DEFAULT 'csv' NOT NULL,
    status varchar DEFAULT 'pending' NOT NULL, -- pending | processing | completed | failed
    content text,
    row_count integer DEFAULT 0 NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS wallet_recurring_rules (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    wallet_id uuid NOT NULL,
    rule_type varchar NOT NULL, -- interval | threshold
    interval varchar,          -- weekly | monthly | quarterly | yearly
    threshold_credits numeric(30,5),
    paid_credits numeric(30,5) DEFAULT 0.0 NOT NULL,
    granted_credits numeric(30,5) DEFAULT 0.0 NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
