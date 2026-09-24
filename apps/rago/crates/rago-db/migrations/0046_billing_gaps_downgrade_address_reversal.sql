-- Billing gaps: customer billing address, scheduled downgrades and reversal of
-- real-time (Mint) captures on void.

-- 1. Customer billing address + identity number (Lago's customer address
--    fields). Iyzico requires a real buyer address and identity number on
--    every checkout form; they are read from here, never from placeholders.
ALTER TABLE customers ADD COLUMN IF NOT EXISTS address_line1 varchar;
ALTER TABLE customers ADD COLUMN IF NOT EXISTS address_line2 varchar;
ALTER TABLE customers ADD COLUMN IF NOT EXISTS city varchar;
ALTER TABLE customers ADD COLUMN IF NOT EXISTS state varchar;
ALTER TABLE customers ADD COLUMN IF NOT EXISTS zipcode varchar;
ALTER TABLE customers ADD COLUMN IF NOT EXISTS phone varchar;
-- Personal identity number (e.g. Turkish TC kimlik no), distinct from a
-- company tax id.
ALTER TABLE customers ADD COLUMN IF NOT EXISTS identity_number varchar;

-- 2. Scheduled downgrades: a pending subscription pointing at the active one
--    it replaces at the next billing boundary.
CREATE INDEX IF NOT EXISTS index_subscriptions_scheduled_downgrades
    ON subscriptions (previous_subscription_id, subscription_at)
    WHERE status = 0 AND previous_subscription_id IS NOT NULL;

-- 3. Real-time wallet captures are moved back to the customer's Mint account
--    when their invoice is voided.
ALTER TABLE wallet_realtime_holds ADD COLUMN IF NOT EXISTS reversed_micros bigint NOT NULL DEFAULT 0;
ALTER TABLE wallet_realtime_holds ADD COLUMN IF NOT EXISTS reversed_at timestamp(6);
ALTER TABLE wallet_realtime_holds DROP CONSTRAINT IF EXISTS wallet_realtime_holds_status_check;
ALTER TABLE wallet_realtime_holds ADD CONSTRAINT wallet_realtime_holds_status_check
    CHECK (status IN ('open', 'captured', 'released', 'flagged', 'reversed'));
CREATE INDEX IF NOT EXISTS index_wallet_realtime_holds_on_wallet_created
    ON wallet_realtime_holds (wallet_id, created_at DESC);
