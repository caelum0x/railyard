-- Entitlements: features → privileges → entitlements (feature on plan/sub) → values.
CREATE TABLE IF NOT EXISTS entitlement_features (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    code varchar NOT NULL,
    name varchar,
    description varchar,
    deleted_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS entitlement_privileges (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    entitlement_feature_id uuid NOT NULL,
    code varchar NOT NULL,
    name varchar,
    value_type varchar DEFAULT 'string' NOT NULL,
    config jsonb DEFAULT '{}' NOT NULL,
    deleted_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS entitlement_entitlements (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    entitlement_feature_id uuid NOT NULL,
    plan_id uuid,
    subscription_id uuid,
    deleted_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS entitlement_entitlement_values (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    entitlement_privilege_id uuid NOT NULL,
    entitlement_entitlement_id uuid NOT NULL,
    value varchar NOT NULL,
    deleted_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

-- Provider mandate id on the customer↔provider link (GoCardless).
ALTER TABLE payment_provider_customers ADD COLUMN IF NOT EXISTS provider_mandate_id varchar;
