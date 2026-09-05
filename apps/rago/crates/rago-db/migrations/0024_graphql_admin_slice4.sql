-- GraphQL admin CRUD, slice 4: catalog v2 + auth/RBAC. Real auth tables (users,
-- memberships, roles, invites, password_resets) store only password *digests*
-- (argon2 PHC strings) — never plaintext. Catalog-v2 tables follow Lago's
-- product/rate-card model.

-- ---- auth / RBAC ----------------------------------------------------------

CREATE TABLE IF NOT EXISTS users (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    email varchar NOT NULL,
    password_digest varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS index_users_on_email ON users (lower(email));

CREATE TABLE IF NOT EXISTS memberships (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    user_id uuid NOT NULL,
    role varchar DEFAULT 'admin' NOT NULL,
    roles jsonb DEFAULT '[]' NOT NULL,
    status varchar DEFAULT 'active' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    revoked_at timestamp(6)
);
CREATE INDEX IF NOT EXISTS index_memberships_on_org ON memberships (organization_id);

CREATE TABLE IF NOT EXISTS roles (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    code varchar NOT NULL,
    name varchar NOT NULL,
    description varchar,
    permissions jsonb DEFAULT '[]' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS invites (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    email varchar NOT NULL,
    token varchar NOT NULL,
    roles jsonb DEFAULT '[]' NOT NULL,
    status varchar DEFAULT 'pending' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    accepted_at timestamp(6),
    revoked_at timestamp(6)
);
CREATE UNIQUE INDEX IF NOT EXISTS index_invites_on_token ON invites (token);

CREATE TABLE IF NOT EXISTS password_resets (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    user_id uuid NOT NULL,
    token varchar NOT NULL,
    expire_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS index_password_resets_on_token ON password_resets (token);

-- billing-entity ↔ tax links (BillingEntities::Taxes::ApplyTaxesService).
CREATE TABLE IF NOT EXISTS billing_entity_taxes (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    billing_entity_id uuid NOT NULL,
    tax_id uuid NOT NULL,
    created_at timestamp(6) NOT NULL
);
CREATE UNIQUE INDEX IF NOT EXISTS index_billing_entity_taxes ON billing_entity_taxes (billing_entity_id, tax_id);

-- ---- catalog v2 -----------------------------------------------------------

CREATE TABLE IF NOT EXISTS product_categories (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    code varchar NOT NULL,
    name varchar NOT NULL,
    description varchar,
    invoice_display_name varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS products (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    code varchar NOT NULL,
    name varchar NOT NULL,
    description varchar,
    invoice_display_name varchar,
    product_type varchar,
    billable_metric_id uuid,
    product_category_id uuid,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS product_filters (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    product_id uuid NOT NULL,
    code varchar NOT NULL,
    name varchar NOT NULL,
    description varchar,
    invoice_display_name varchar,
    values jsonb DEFAULT '[]' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS rate_cards (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    product_id uuid NOT NULL,
    code varchar NOT NULL,
    name varchar NOT NULL,
    description varchar,
    currency varchar NOT NULL,
    billing_timing varchar,
    proration boolean DEFAULT false NOT NULL,
    display_on_invoice boolean DEFAULT true NOT NULL,
    wallet_targetable boolean DEFAULT false NOT NULL,
    product_filter_id uuid,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS rate_card_rates (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    rate_card_id uuid NOT NULL,
    charge_model integer DEFAULT 0 NOT NULL,
    properties jsonb DEFAULT '{}' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS plan_applied_rate_cards (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    plan_id uuid NOT NULL,
    rate_card_id uuid NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS rate_phases (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    plan_applied_rate_card_id uuid NOT NULL,
    code varchar NOT NULL,
    name varchar,
    position integer,
    billing_interval_cycle_count integer,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS pricing_units (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    code varchar NOT NULL,
    name varchar NOT NULL,
    short_name varchar NOT NULL,
    description varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);

CREATE TABLE IF NOT EXISTS commitments (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    plan_id uuid,
    commitment_type varchar DEFAULT 'minimum_commitment' NOT NULL,
    amount_cents bigint DEFAULT 0 NOT NULL,
    invoice_display_name varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);
