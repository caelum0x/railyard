-- Idempotency / dedup guards (money-correctness).
--
-- These indexes make three billing operations idempotent under scheduler/job
-- re-runs and concurrent identical requests, mirroring Lago's own constraints.

-- 1. Usage-event dedup — the DB is the single source of truth. Faithful to Lago's
--    `index_unique_transaction_id` (getlago/lago db/structure.sql): an event is
--    unique per (organization, external_subscription, transaction). This turns the
--    former racy check-then-insert into an atomic `ON CONFLICT DO NOTHING`.
CREATE UNIQUE INDEX IF NOT EXISTS index_unique_transaction_id
    ON events (organization_id, external_subscription_id, transaction_id);

-- 2. Re-bill guard — at most one recurring invoice per (subscription, charges
--    period). Faithful to Lago's
--    `index_uniq_invoice_subscriptions_on_charges_from_to_datetime`. A scheduler
--    re-run for the same subscription+period can no longer create a second
--    invoice (which, with the payment path, would double-charge).
CREATE UNIQUE INDEX IF NOT EXISTS index_uniq_invoice_subscriptions_on_charges_from_to_datetime
    ON invoice_subscriptions (subscription_id, charges_from_datetime, charges_to_datetime)
    WHERE recurring IS TRUE;
