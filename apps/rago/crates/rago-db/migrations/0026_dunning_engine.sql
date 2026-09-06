-- Dunning engine (automated failed-payment recovery).
--
-- Ports the schema behind Lago's `DunningCampaigns::BulkProcessService` /
-- `ProcessAttemptService` / `PaymentRequests::CreateService`:
--   * per-currency campaign thresholds (`dunning_campaign_thresholds`),
--   * per-customer, per-currency attempt tracking + exclusion
--     (`customers.dunning_currency_attempts` / `last_dunning_campaign_attempt_at`
--     / `exclude_from_dunning_campaign` / `applied_dunning_campaign_id`),
--   * the payment_request ↔ invoice link (`payment_request_applied_invoices`)
--     and the payment_request → dunning_campaign back-reference.
-- Mirrors the shapes in lago/api/db/schema.rb.

-- Per-currency thresholds for a campaign. A currency's overdue balance must
-- reach the highest threshold at-or-below it to trigger a dunning attempt.
CREATE TABLE IF NOT EXISTS dunning_campaign_thresholds (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    dunning_campaign_id uuid NOT NULL,
    organization_id uuid NOT NULL,
    currency varchar NOT NULL,
    amount_cents bigint NOT NULL,
    created_at timestamp(6) NOT NULL DEFAULT now(),
    updated_at timestamp(6) NOT NULL DEFAULT now()
);
-- One threshold per (campaign, currency) — mirrors Lago's uniqueness.
CREATE UNIQUE INDEX IF NOT EXISTS idx_dct_campaign_currency
    ON dunning_campaign_thresholds (dunning_campaign_id, currency);

-- Per-customer dunning state (Lago `customers` columns).
ALTER TABLE customers ADD COLUMN IF NOT EXISTS dunning_currency_attempts jsonb DEFAULT '{}' NOT NULL;
ALTER TABLE customers ADD COLUMN IF NOT EXISTS last_dunning_campaign_attempt_at timestamp(6);
ALTER TABLE customers ADD COLUMN IF NOT EXISTS exclude_from_dunning_campaign boolean DEFAULT false NOT NULL;
-- Customer-level campaign override (else the org default campaign applies).
ALTER TABLE customers ADD COLUMN IF NOT EXISTS applied_dunning_campaign_id uuid;

-- The invoices a payment_request is trying to collect (Lago
-- `PaymentRequest::AppliedInvoice`).
CREATE TABLE IF NOT EXISTS payment_request_applied_invoices (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    payment_request_id uuid NOT NULL,
    invoice_id uuid NOT NULL,
    organization_id uuid NOT NULL,
    created_at timestamp(6) NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_praiv_request_invoice
    ON payment_request_applied_invoices (payment_request_id, invoice_id);
CREATE INDEX IF NOT EXISTS idx_praiv_invoice
    ON payment_request_applied_invoices (invoice_id);

-- The campaign that produced an automated payment_request (nullable: manual
-- payment requests via the REST API have none).
ALTER TABLE payment_requests ADD COLUMN IF NOT EXISTS dunning_campaign_id uuid;
