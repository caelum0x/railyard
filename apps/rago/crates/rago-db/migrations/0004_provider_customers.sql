-- Link a Lago customer to a payment-provider customer (Stripe `cus_…`).
CREATE TABLE IF NOT EXISTS payment_provider_customers (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    customer_id uuid NOT NULL,
    provider varchar NOT NULL,
    provider_customer_id varchar,
    payment_method_id varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_ppc_customer ON payment_provider_customers (customer_id, provider);
