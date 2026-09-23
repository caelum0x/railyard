-- Provider refunds, SSO OAuth state, and per-organization inbound webhook
-- secrets.

-- Refunds issued at the payment provider (invoice refunds + credit-note
-- refunds). `status` is pending | succeeded | failed; the final outcome of an
-- asynchronous refund (GoCardless, Adyen) arrives via the provider webhook.
CREATE TABLE IF NOT EXISTS refunds (
    id uuid NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    invoice_id uuid NOT NULL,
    payment_id uuid NOT NULL,
    credit_note_id uuid,
    provider varchar NOT NULL,
    provider_refund_id varchar,
    provider_status varchar,
    status varchar NOT NULL,
    amount_cents bigint NOT NULL,
    amount_currency varchar NOT NULL,
    last_error text,
    created_at timestamp(6) NOT NULL DEFAULT now(),
    updated_at timestamp(6) NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_refunds_payment_id ON refunds (payment_id);
CREATE INDEX IF NOT EXISTS idx_refunds_invoice_id ON refunds (invoice_id);
CREATE INDEX IF NOT EXISTS idx_refunds_provider_refund_id ON refunds (provider_refund_id);
-- One provider refund per credit note.
CREATE UNIQUE INDEX IF NOT EXISTS idx_refunds_credit_note_id ON refunds (credit_note_id) WHERE credit_note_id IS NOT NULL;

-- Inbound webhook signing secret per payment provider (Stripe `whsec_…`,
-- GoCardless webhook secret, Adyen HMAC key), sealed like provider API keys.
ALTER TABLE payment_providers ADD COLUMN IF NOT EXISTS encrypted_webhook_secret text;
ALTER TABLE payment_providers ADD COLUMN IF NOT EXISTS webhook_secret_ref varchar;

-- Single-use SSO authorization state (login-CSRF protection + PKCE verifier).
CREATE TABLE IF NOT EXISTS sso_states (
    state varchar NOT NULL PRIMARY KEY,
    provider varchar NOT NULL,
    code_verifier varchar NOT NULL,
    expires_at timestamp(6) NOT NULL,
    consumed_at timestamp(6),
    created_at timestamp(6) NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS idx_sso_states_expires_at ON sso_states (expires_at);
