-- Billing correctness: closing-usage invoices on downgrade, cancel at period
-- end from the customer portal, and the Iyzico buyer IP.

-- 1. Why an invoice_subscriptions row exists (Lago's `invoicing_reason`).
--    Recurring rows (`subscription_periodic`) keep the existing re-bill guard.
--    A pay-in-advance subscription that leaves through a downgrade is billed
--    the usage of its closing period once (`subscription_terminating`); its
--    fee for that period was already collected in advance, under a recurring
--    row keyed by the same dates, so it needs its own unique guard.
ALTER TABLE invoice_subscriptions ADD COLUMN IF NOT EXISTS invoicing_reason varchar;
CREATE UNIQUE INDEX IF NOT EXISTS index_uniq_invoice_subscriptions_closing_charges
    ON invoice_subscriptions (subscription_id, charges_from_datetime, charges_to_datetime)
    WHERE recurring IS FALSE AND invoicing_reason = 'subscription_terminating';

-- 2. Customer-requested cancellation at the end of the current period.
--    `ending_at` (already honoured by the TerminateEndedSubscriptions clock)
--    carries the date; this flag records that the customer asked for it from
--    the portal so it can be undone there (an operator-set end date cannot).
ALTER TABLE subscriptions ADD COLUMN IF NOT EXISTS cancel_at_period_end boolean NOT NULL DEFAULT false;

-- 3. Last known public IP of the customer (captured on portal/checkout
--    requests). Iyzico requires a buyer IP on every payment; clock- and
--    portal-initiated charges have no live request, so they use this one.
ALTER TABLE customers ADD COLUMN IF NOT EXISTS last_known_ip varchar;
ALTER TABLE customers ADD COLUMN IF NOT EXISTS last_known_ip_at timestamp(6);
