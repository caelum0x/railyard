-- Production payment collection + webhook delivery bookkeeping.

-- Payment-provider secrets saved from the dashboard, sealed with AES-256-GCM
-- under RAGO_ENCRYPTION_KEY (see `rago_db::secrets`). `secret_ref` keeps the
-- masked `****last4` display value.
ALTER TABLE payment_providers ADD COLUMN IF NOT EXISTS encrypted_secrets text;

-- Per-payment attempt counter: each legitimate retry of a FAILED charge uses a
-- fresh provider idempotency key (`payment-<id>-<attempt>`).
ALTER TABLE payments ADD COLUMN IF NOT EXISTS attempts integer DEFAULT 0 NOT NULL;
ALTER TABLE payments ADD COLUMN IF NOT EXISTS last_error text;

-- Stored payment method per provider customer (already present on older
-- schemas; kept idempotent).
ALTER TABLE payment_provider_customers ADD COLUMN IF NOT EXISTS payment_method_id varchar;

-- Link each delivery-feed row to the `webhooks` work item it reports on, so the
-- delivery worker can update the operator view with the real outcome.
ALTER TABLE webhook_deliveries ADD COLUMN IF NOT EXISTS webhook_id uuid;
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_webhook_id ON webhook_deliveries (webhook_id);

-- Delivery worker scan: pending + retrying work items by due time.
CREATE INDEX IF NOT EXISTS idx_webhooks_status_next_retry ON webhooks (status, next_retry_at);

-- Once-per-period daemon claims (e.g. the daily billing cycle), so a tick
-- loop running every few seconds enqueues the cycle exactly once per day even
-- across several server replicas.
CREATE TABLE IF NOT EXISTS daemon_runs (
    name varchar NOT NULL PRIMARY KEY,
    last_run_on date NOT NULL,
    updated_at timestamp(6) NOT NULL DEFAULT now()
);

-- Invoice metadata (`PUT /invoices/:id` { metadata }), merged key/value pairs.
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS metadata jsonb DEFAULT '{}' NOT NULL;
