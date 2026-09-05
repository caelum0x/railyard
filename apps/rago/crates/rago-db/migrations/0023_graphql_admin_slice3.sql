-- GraphQL admin CRUD, slice 3: the external-service config surface (payment
-- providers + integrations orchestration). These rows store PROVIDER CONFIG and
-- MAPPINGS only — never raw secrets. Secret material is referenced by a masked
-- `secret_ref` (e.g. ****4242); the real credential is owner-provisioned to the
-- transport layer (rago-integrations) out of band.

-- Payment-provider configurations (Stripe/Adyen/GoCardless/Cashfree/Flutterwave/
-- Moneyhash). One row per (organization, code).
CREATE TABLE IF NOT EXISTS payment_providers (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    provider_type varchar NOT NULL,
    code varchar NOT NULL,
    name varchar,
    secret_ref varchar,
    success_redirect_url varchar,
    settings jsonb DEFAULT '{}' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);
CREATE INDEX IF NOT EXISTS index_payment_providers_on_org ON payment_providers (organization_id);

-- payment_provider_customers gains dashboard-managed columns.
ALTER TABLE payment_provider_customers ADD COLUMN IF NOT EXISTS payment_provider_code varchar;
ALTER TABLE payment_provider_customers ADD COLUMN IF NOT EXISTS settings jsonb DEFAULT '{}' NOT NULL;
ALTER TABLE payment_provider_customers ADD COLUMN IF NOT EXISTS is_default boolean DEFAULT false NOT NULL;
ALTER TABLE payment_provider_customers ADD COLUMN IF NOT EXISTS deleted_at timestamp(6);

-- Customer payment methods (cards/mandates stored at the provider).
CREATE TABLE IF NOT EXISTS payment_methods (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    customer_id uuid NOT NULL,
    provider_method_id varchar,
    is_default boolean DEFAULT false NOT NULL,
    details jsonb DEFAULT '{}' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

-- Payment requests (dunning: ask a customer to pay a set of invoices).
CREATE TABLE IF NOT EXISTS payment_requests (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    customer_id uuid,
    external_customer_id varchar,
    email varchar,
    amount_cents bigint DEFAULT 0 NOT NULL,
    amount_currency varchar,
    payment_status varchar DEFAULT 'pending' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

-- integration_connections gains a dashboard code + soft-delete.
ALTER TABLE integration_connections ADD COLUMN IF NOT EXISTS code varchar;
ALTER TABLE integration_connections ADD COLUMN IF NOT EXISTS deleted_at timestamp(6);

-- integration_mappings gains the external account/name columns + timestamps.
ALTER TABLE integration_mappings ADD COLUMN IF NOT EXISTS external_account_code varchar;
ALTER TABLE integration_mappings ADD COLUMN IF NOT EXISTS external_name varchar;
ALTER TABLE integration_mappings ADD COLUMN IF NOT EXISTS updated_at timestamp(6);
ALTER TABLE integration_mappings ADD COLUMN IF NOT EXISTS deleted_at timestamp(6);

-- Integration ↔ customer connections.
CREATE TABLE IF NOT EXISTS integration_customers (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    integration_id uuid NOT NULL,
    customer_id uuid NOT NULL,
    external_customer_id varchar,
    code varchar,
    is_default boolean DEFAULT false NOT NULL,
    settings jsonb DEFAULT '{}' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

-- Integration collection mappings (map a Lago concept — e.g. fallback item,
-- coupon, tax — to an external account/ledger code).
CREATE TABLE IF NOT EXISTS integration_collection_mappings (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    integration_id uuid NOT NULL,
    mapping_type varchar NOT NULL,
    external_id varchar,
    external_account_code varchar,
    external_name varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

-- Fetched integration items (populated by the transport fetch; empty until a
-- live sync runs with owner-provided credentials).
CREATE TABLE IF NOT EXISTS integration_items (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    integration_id uuid NOT NULL,
    external_id varchar NOT NULL,
    item_type varchar,
    external_name varchar,
    external_account_code varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
