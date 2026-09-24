-- Real-time prepaid wallets backed by Mint holds.
--
-- A wallet with `realtime_enabled` keeps its money in the customer's Mint
-- account (`mint_account_id`). Every billable event is priced on arrival and
-- reserved with a Mint hold; `realtime_policy` decides what happens when the
-- customer cannot cover it (`reject` the event, or store it and `flag` the
-- shortfall). Holds are captured to the seller when the invoice is finalized and
-- released on void.

ALTER TABLE wallets ADD COLUMN IF NOT EXISTS realtime_enabled boolean NOT NULL DEFAULT false;
ALTER TABLE wallets ADD COLUMN IF NOT EXISTS realtime_policy varchar NOT NULL DEFAULT 'reject';
ALTER TABLE wallets ADD COLUMN IF NOT EXISTS mint_account_id varchar;

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'wallets_realtime_policy_check') THEN
        ALTER TABLE wallets ADD CONSTRAINT wallets_realtime_policy_check
            CHECK (realtime_policy IN ('reject', 'flag'));
    END IF;
    IF NOT EXISTS (SELECT 1 FROM pg_constraint WHERE conname = 'wallets_realtime_needs_mint_account') THEN
        ALTER TABLE wallets ADD CONSTRAINT wallets_realtime_needs_mint_account
            CHECK (NOT realtime_enabled OR (mint_account_id IS NOT NULL AND mint_account_id <> ''));
    END IF;
END $$;

-- One row per (wallet, event transaction): the Mint hold that reserved the
-- event's price, or a `flagged` row when the customer could not cover it.
CREATE TABLE IF NOT EXISTS wallet_realtime_holds (
    id uuid PRIMARY KEY,
    organization_id uuid NOT NULL,
    wallet_id uuid NOT NULL,
    customer_id uuid NOT NULL,
    subscription_id uuid NOT NULL,
    event_transaction_id varchar NOT NULL,
    event_timestamp timestamp(6) NOT NULL,
    mint_hold_id varchar,
    amount_cents bigint NOT NULL,
    amount_micros bigint NOT NULL,
    status varchar NOT NULL,
    invoice_id uuid,
    captured_micros bigint NOT NULL DEFAULT 0,
    last_error varchar,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    CONSTRAINT wallet_realtime_holds_status_check
        CHECK (status IN ('open', 'captured', 'released', 'flagged'))
);
CREATE UNIQUE INDEX IF NOT EXISTS index_wallet_realtime_holds_on_wallet_and_event
    ON wallet_realtime_holds (wallet_id, event_transaction_id);
CREATE INDEX IF NOT EXISTS index_wallet_realtime_holds_open
    ON wallet_realtime_holds (subscription_id, created_at) WHERE status = 'open';
CREATE INDEX IF NOT EXISTS index_wallet_realtime_holds_on_invoice
    ON wallet_realtime_holds (invoice_id) WHERE invoice_id IS NOT NULL;
