-- Creem, Dodo Payments and Iyzico become first-class invoice payment
-- providers next to Stripe, GoCardless and Adyen.
--
-- 1. `payment_providers.provider_type` is constrained to the providers Rago can
--    actually collect, refund and receive webhooks for. The constraint is added
--    NOT VALID so historical rows are left untouched; every new or updated row
--    is checked.
-- 2. Hosted-checkout providers (Creem, Dodo, Iyzico) create the payment under a
--    checkout/session/token id and settle it under the provider's payment id.
--    `payments.provider_checkout_id` keeps the original checkout reference after
--    the webhook rebinds `provider_payment_id`, so late or replayed deliveries
--    keyed by the checkout still find the payment.
-- 3. `provider_webhook_events` records every processed delivery id per
--    (organization, provider) so a replayed webhook is acknowledged without
--    being applied twice.

DO $$
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_constraint WHERE conname = 'payment_providers_provider_type_check'
    ) THEN
        ALTER TABLE payment_providers
            ADD CONSTRAINT payment_providers_provider_type_check
            CHECK (provider_type IN ('stripe', 'gocardless', 'adyen', 'creem', 'dodo', 'iyzico'))
            NOT VALID;
    END IF;
END $$;

ALTER TABLE payments ADD COLUMN IF NOT EXISTS provider_checkout_id varchar;
CREATE INDEX IF NOT EXISTS index_payments_on_provider_payment_id
    ON payments (provider_payment_id) WHERE provider_payment_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS index_payments_on_provider_checkout_id
    ON payments (provider_checkout_id) WHERE provider_checkout_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS provider_webhook_events (
    organization_id uuid NOT NULL,
    provider varchar NOT NULL,
    event_id varchar NOT NULL,
    event_type varchar,
    received_at timestamp(6) NOT NULL DEFAULT now(),
    PRIMARY KEY (organization_id, provider, event_id)
);
CREATE INDEX IF NOT EXISTS index_provider_webhook_events_on_received_at
    ON provider_webhook_events (received_at);
