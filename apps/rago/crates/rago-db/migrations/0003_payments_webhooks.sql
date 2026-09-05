-- Payments + outbound webhook delivery records.

-- Organizations gain an HMAC key used to sign webhooks (Lago `hmac_key`).
ALTER TABLE organizations ADD COLUMN IF NOT EXISTS hmac_key varchar;
UPDATE organizations SET hmac_key = gen_random_uuid()::text WHERE hmac_key IS NULL;

CREATE TABLE IF NOT EXISTS payments (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    invoice_id uuid NOT NULL,
    provider varchar NOT NULL,
    provider_payment_id varchar,
    provider_status varchar,
    status varchar NOT NULL,
    amount_cents bigint NOT NULL,
    amount_currency varchar NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);

CREATE TABLE IF NOT EXISTS webhooks (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    webhook_type varchar NOT NULL,
    object_type varchar NOT NULL,
    endpoint varchar NOT NULL,
    payload jsonb DEFAULT '{}' NOT NULL,
    status varchar DEFAULT 'pending' NOT NULL,
    http_status integer,
    response text,
    retries integer DEFAULT 0 NOT NULL,
    last_retried_at timestamp(6),
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS index_webhooks_on_status ON webhooks (status);

CREATE TABLE IF NOT EXISTS webhook_endpoints (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    webhook_url varchar NOT NULL,
    signature_algo varchar DEFAULT 'hmac' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
