-- Core billing schema — a faithful subset of Lago's db/structure.sql covering
-- the entities the billing vertical persists. Column names and Postgres types
-- match Lago exactly so the port stays wire-compatible with a real Lago DB.

CREATE TABLE IF NOT EXISTS organizations (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    name varchar NOT NULL,
    slug varchar NOT NULL,
    default_currency varchar DEFAULT 'USD' NOT NULL,
    email varchar,
    country varchar,
    tax_identification_number varchar,
    timezone varchar DEFAULT 'UTC' NOT NULL,
    document_locale varchar DEFAULT 'en' NOT NULL,
    finalize_zero_amount_invoice boolean DEFAULT true NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS customers (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    billing_entity_id uuid NOT NULL,
    external_id varchar NOT NULL,
    name varchar,
    email varchar,
    country varchar,
    currency varchar,
    timezone varchar,
    tax_identification_number varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS billable_metrics (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    name varchar NOT NULL,
    code varchar NOT NULL,
    description varchar,
    aggregation_type integer NOT NULL,
    field_name varchar,
    recurring boolean DEFAULT false NOT NULL,
    expression varchar,
    rounding_function varchar,
    rounding_precision integer,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS plans (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    name varchar NOT NULL,
    code varchar NOT NULL,
    interval integer,
    description varchar,
    amount_cents bigint,
    amount_currency varchar NOT NULL,
    trial_period double precision,
    pay_in_advance boolean DEFAULT false,
    bill_charges_monthly boolean,
    parent_id uuid,
    invoice_display_name varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS charges (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    billable_metric_id uuid,
    plan_id uuid,
    code varchar NOT NULL,
    charge_model integer DEFAULT 0 NOT NULL,
    properties jsonb DEFAULT '{}' NOT NULL,
    pay_in_advance boolean DEFAULT false NOT NULL,
    prorated boolean DEFAULT false NOT NULL,
    invoiceable boolean DEFAULT true NOT NULL,
    min_amount_cents bigint DEFAULT 0 NOT NULL,
    amount_currency varchar,
    accepts_target_wallet boolean DEFAULT false NOT NULL,
    invoice_display_name varchar,
    parent_id uuid,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS subscriptions (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    customer_id uuid NOT NULL,
    plan_id uuid NOT NULL,
    billing_entity_id uuid,
    external_id varchar NOT NULL,
    name varchar,
    status integer NOT NULL,
    billing_time integer DEFAULT 0 NOT NULL,
    subscription_at timestamp(6),
    started_at timestamp,
    ending_at timestamp(6),
    trial_ended_at timestamp(6),
    canceled_at timestamp,
    terminated_at timestamp,
    previous_subscription_id uuid,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS taxes (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    name varchar NOT NULL,
    code varchar NOT NULL,
    description varchar,
    rate double precision DEFAULT 0.0 NOT NULL,
    applied_to_organization boolean DEFAULT false NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS events (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    customer_id uuid,
    subscription_id uuid,
    transaction_id varchar NOT NULL,
    code varchar NOT NULL,
    properties jsonb DEFAULT '{}' NOT NULL,
    external_customer_id varchar,
    external_subscription_id varchar,
    timestamp timestamp,
    precise_total_amount_cents numeric(40,15),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS invoices (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    billing_entity_id uuid NOT NULL,
    customer_id uuid,
    number varchar DEFAULT '' NOT NULL,
    sequential_id integer,
    invoice_type integer DEFAULT 0 NOT NULL,
    status integer DEFAULT 1 NOT NULL,
    payment_status integer DEFAULT 0 NOT NULL,
    currency varchar,
    issuing_date date,
    fees_amount_cents bigint DEFAULT 0 NOT NULL,
    coupons_amount_cents bigint DEFAULT 0 NOT NULL,
    credit_notes_amount_cents bigint DEFAULT 0 NOT NULL,
    prepaid_credit_amount_cents bigint DEFAULT 0 NOT NULL,
    progressive_billing_credit_amount_cents bigint DEFAULT 0 NOT NULL,
    sub_total_excluding_taxes_amount_cents bigint DEFAULT 0 NOT NULL,
    sub_total_including_taxes_amount_cents bigint DEFAULT 0 NOT NULL,
    taxes_amount_cents bigint DEFAULT 0 NOT NULL,
    taxes_rate double precision DEFAULT 0.0 NOT NULL,
    total_amount_cents bigint DEFAULT 0 NOT NULL,
    version_number integer DEFAULT 4 NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS fees (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    billing_entity_id uuid NOT NULL,
    invoice_id uuid,
    subscription_id uuid,
    charge_id uuid,
    charge_filter_id uuid,
    fee_type integer,
    invoiceable_type varchar,
    invoiceable_id uuid,
    amount_cents bigint NOT NULL,
    amount_currency varchar NOT NULL,
    precise_amount_cents numeric(40,15) DEFAULT 0.0 NOT NULL,
    unit_amount_cents bigint DEFAULT 0 NOT NULL,
    precise_unit_amount numeric(30,15) DEFAULT 0.0 NOT NULL,
    taxes_amount_cents bigint NOT NULL,
    taxes_precise_amount_cents numeric(40,15) DEFAULT 0.0 NOT NULL,
    taxes_rate double precision DEFAULT 0.0 NOT NULL,
    units numeric DEFAULT 0.0 NOT NULL,
    total_aggregated_units numeric,
    events_count integer,
    precise_coupons_amount_cents numeric(30,5) DEFAULT 0.0 NOT NULL,
    precise_credit_notes_amount_cents numeric(30,5) DEFAULT 0.0 NOT NULL,
    payment_status integer DEFAULT 0 NOT NULL,
    properties jsonb DEFAULT '{}' NOT NULL,
    amount_details jsonb DEFAULT '{}' NOT NULL,
    grouped_by jsonb DEFAULT '{}' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
