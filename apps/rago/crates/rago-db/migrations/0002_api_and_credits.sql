-- API keys + the wallet/coupon/credit-note tables the REST API exposes.
-- Faithful subset of Lago's db/structure.sql.

CREATE TABLE IF NOT EXISTS api_keys (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    value varchar NOT NULL,
    name varchar,
    permissions jsonb DEFAULT '{}' NOT NULL,
    expires_at timestamp(6),
    last_used_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS index_api_keys_on_value ON api_keys (value);

CREATE TABLE IF NOT EXISTS wallets (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    customer_id uuid NOT NULL,
    status integer NOT NULL,
    name varchar,
    code varchar,
    rate_amount numeric(30,5) DEFAULT 0.0 NOT NULL,
    credits_balance numeric(30,5) DEFAULT 0.0 NOT NULL,
    consumed_credits numeric(30,5) DEFAULT 0.0 NOT NULL,
    balance_cents bigint DEFAULT 0 NOT NULL,
    balance_currency varchar NOT NULL,
    consumed_amount_cents bigint DEFAULT 0 NOT NULL,
    ongoing_balance_cents bigint DEFAULT 0 NOT NULL,
    ongoing_usage_balance_cents bigint DEFAULT 0 NOT NULL,
    credits_ongoing_balance numeric(30,5) DEFAULT 0.0 NOT NULL,
    credits_ongoing_usage_balance numeric(30,5) DEFAULT 0.0 NOT NULL,
    expiration_at timestamp,
    terminated_at timestamp,
    lock_version integer DEFAULT 0 NOT NULL,
    priority integer DEFAULT 50 NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS wallet_transactions (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    wallet_id uuid NOT NULL,
    transaction_type integer NOT NULL,
    status integer NOT NULL,
    amount numeric(30,5) DEFAULT 0.0 NOT NULL,
    credit_amount numeric(30,5) DEFAULT 0.0 NOT NULL,
    invoice_id uuid,
    settled_at timestamp,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS coupons (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    name varchar NOT NULL,
    code varchar NOT NULL,
    status integer DEFAULT 0 NOT NULL,
    coupon_type integer DEFAULT 0 NOT NULL,
    amount_cents bigint,
    amount_currency varchar,
    percentage_rate numeric(10,5),
    frequency integer DEFAULT 0 NOT NULL,
    frequency_duration integer,
    expiration integer DEFAULT 0 NOT NULL,
    expiration_at timestamp(6),
    reusable boolean DEFAULT true NOT NULL,
    limited_plans boolean DEFAULT false NOT NULL,
    limited_billable_metrics boolean DEFAULT false NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS applied_coupons (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    coupon_id uuid NOT NULL,
    customer_id uuid NOT NULL,
    status integer DEFAULT 0 NOT NULL,
    amount_cents bigint,
    amount_currency varchar,
    percentage_rate numeric(10,5),
    frequency integer DEFAULT 0 NOT NULL,
    frequency_duration integer,
    frequency_duration_remaining integer,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    terminated_at timestamp
);

CREATE TABLE IF NOT EXISTS credits (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    invoice_id uuid,
    applied_coupon_id uuid,
    credit_note_id uuid,
    progressive_billing_invoice_id uuid,
    amount_cents bigint NOT NULL,
    amount_currency varchar NOT NULL,
    before_taxes boolean DEFAULT false NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS credit_notes (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    customer_id uuid NOT NULL,
    invoice_id uuid NOT NULL,
    sequential_id integer NOT NULL,
    number varchar NOT NULL,
    issuing_date date NOT NULL,
    reason integer NOT NULL,
    credit_status integer,
    refund_status integer,
    status integer DEFAULT 1 NOT NULL,
    credit_amount_cents bigint DEFAULT 0 NOT NULL,
    credit_amount_currency varchar NOT NULL,
    balance_amount_cents bigint DEFAULT 0 NOT NULL,
    refund_amount_cents bigint DEFAULT 0 NOT NULL,
    total_amount_cents bigint DEFAULT 0 NOT NULL,
    total_amount_currency varchar NOT NULL,
    taxes_amount_cents bigint DEFAULT 0 NOT NULL,
    taxes_rate double precision DEFAULT 0.0 NOT NULL,
    coupons_adjustment_amount_cents bigint DEFAULT 0 NOT NULL,
    precise_coupons_adjustment_amount_cents numeric(30,5) DEFAULT 0.0 NOT NULL,
    precise_taxes_amount_cents numeric(30,5) DEFAULT 0.0 NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    voided_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS invoice_subscriptions (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    invoice_id uuid NOT NULL,
    subscription_id uuid NOT NULL,
    recurring boolean,
    "timestamp" timestamp(6),
    from_datetime timestamp(6),
    to_datetime timestamp(6),
    charges_from_datetime timestamp(6),
    charges_to_datetime timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
