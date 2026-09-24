-- Billing completeness: custom aggregation, per-organization invoice numbering,
-- progressive billing (usage thresholds), and Stripe SCA/3DS next actions.

-- `custom_agg` metrics: the per-event expression whose results are summed into
-- the custom charge amount (same grammar as `expression`).
ALTER TABLE billable_metrics ADD COLUMN IF NOT EXISTS custom_aggregator text;

-- Lago-style invoice numbering: `<prefix>-<YYYYMM>-<org sequence>`.
ALTER TABLE organizations ADD COLUMN IF NOT EXISTS document_number_prefix varchar;

-- One counter row per organization; incremented with an upsert inside the
-- invoice-insert transaction (the row lock serializes concurrent billers, and a
-- rolled-back insert returns its number, so the sequence stays gap-free).
CREATE TABLE IF NOT EXISTS invoice_number_sequences (
    organization_id uuid NOT NULL PRIMARY KEY,
    last_value bigint NOT NULL DEFAULT 0,
    updated_at timestamp(6) NOT NULL DEFAULT now()
);

-- Continue after the invoices that already exist.
INSERT INTO invoice_number_sequences (organization_id, last_value, updated_at)
SELECT organization_id, COUNT(*), now() FROM invoices GROUP BY organization_id
ON CONFLICT (organization_id) DO NOTHING;

-- Legacy invoice numbers (`INV-` + 8 random hex characters, or numbers copied
-- by older clone/regenerate paths) can repeat inside one organization, which
-- would make the unique index below fail and stop the migrator. Renumber the
-- duplicates first: the earliest invoice keeps its number, later ones get
-- `-2`, `-3`, ... A suffixed number could itself collide with an existing one,
-- so the pass repeats until no duplicate is left (each pass strictly reduces
-- the duplicate set, so this converges; the bound only guards against a
-- pathological loop). The row count per organization is unchanged, so the
-- sequence seed above stays correct, and the new `<prefix>-<YYYYMM>-<seq>`
-- format can never collide with the legacy `INV-xxxxxxxx` ones.
DO $$
DECLARE
    passes integer := 0;
BEGIN
    LOOP
        WITH ranked AS (
            SELECT id,
                   number,
                   row_number() OVER (
                       PARTITION BY organization_id, number
                       ORDER BY created_at, id
                   ) AS rn
            FROM invoices
            WHERE number <> ''
        )
        UPDATE invoices i
           SET number = ranked.number || '-' || ranked.rn::text,
               updated_at = now()
          FROM ranked
         WHERE i.id = ranked.id AND ranked.rn > 1;
        EXIT WHEN NOT FOUND;
        passes := passes + 1;
        IF passes >= 20 THEN
            RAISE EXCEPTION 'could not de-duplicate invoice numbers after % passes', passes;
        END IF;
    END LOOP;
END $$;

CREATE UNIQUE INDEX IF NOT EXISTS idx_invoices_org_number
    ON invoices (organization_id, number) WHERE number <> '';

-- Progressive billing thresholds (Lago `usage_thresholds`), attached either to
-- a plan (inherited by its subscriptions) or overridden per subscription.
CREATE TABLE IF NOT EXISTS usage_thresholds (
    id uuid NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    plan_id uuid,
    subscription_id uuid,
    threshold_display_name varchar,
    amount_cents bigint NOT NULL,
    recurring boolean NOT NULL DEFAULT false,
    created_at timestamp(6) NOT NULL DEFAULT now(),
    updated_at timestamp(6) NOT NULL DEFAULT now(),
    deleted_at timestamp(6),
    CHECK (amount_cents > 0),
    CHECK ((plan_id IS NULL) <> (subscription_id IS NULL))
);
CREATE INDEX IF NOT EXISTS idx_usage_thresholds_plan ON usage_thresholds (plan_id) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_usage_thresholds_subscription ON usage_thresholds (subscription_id) WHERE deleted_at IS NULL;
-- At most one recurring threshold per plan / subscription.
CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_thresholds_plan_recurring
    ON usage_thresholds (plan_id) WHERE recurring AND deleted_at IS NULL AND plan_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_usage_thresholds_subscription_recurring
    ON usage_thresholds (subscription_id) WHERE recurring AND deleted_at IS NULL AND subscription_id IS NOT NULL;

-- Which thresholds each progressive invoice was issued for, and the usage
-- amount it billed (the credit-back applied on the period-end invoice).
CREATE TABLE IF NOT EXISTS progressive_billing_invoices (
    id uuid NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    subscription_id uuid NOT NULL,
    invoice_id uuid NOT NULL,
    charges_from_datetime timestamp(6) NOT NULL,
    charges_to_datetime timestamp(6) NOT NULL,
    usage_amount_cents bigint NOT NULL,
    threshold_ids uuid[] NOT NULL DEFAULT '{}',
    credited_invoice_id uuid,
    created_at timestamp(6) NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_progressive_invoices_period
    ON progressive_billing_invoices (subscription_id, charges_from_datetime, charges_to_datetime);

-- Stripe SCA / 3-D Secure: what the customer must do to complete a payment.
ALTER TABLE payments ADD COLUMN IF NOT EXISTS next_action jsonb;
ALTER TABLE payments ADD COLUMN IF NOT EXISTS next_action_url text;

-- Pay-in-advance charges are billed when their event is ingested; one row per
-- (event, charge) makes that exactly-once across job retries.
CREATE TABLE IF NOT EXISTS pay_in_advance_billings (
    event_id uuid NOT NULL,
    charge_id uuid NOT NULL,
    organization_id uuid NOT NULL,
    invoice_id uuid NOT NULL,
    created_at timestamp(6) NOT NULL DEFAULT now(),
    PRIMARY KEY (event_id, charge_id)
);

-- Numbered payment receipts, one per settled payment.
CREATE TABLE IF NOT EXISTS payment_receipts (
    id uuid NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    payment_id uuid NOT NULL UNIQUE,
    invoice_id uuid NOT NULL,
    number varchar NOT NULL,
    amount_cents bigint NOT NULL,
    amount_currency varchar NOT NULL,
    created_at timestamp(6) NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_payment_receipts_invoice ON payment_receipts (invoice_id);

