-- Add allowed_fee_types to wallets.
--
-- Mirrors Lago's wallet#allowed_fee_types: an integer array that restricts
-- which FeeType values (0=charge,1=add_on,2=subscription,3=credit,
-- 4=commitment,5=fixed_charge,6=product) the wallet may pay.
-- An empty array (the default) means no restriction — the wallet pays any
-- fee type (the existing unrestricted behaviour).
ALTER TABLE wallets
    ADD COLUMN IF NOT EXISTS allowed_fee_types integer[] NOT NULL DEFAULT '{}';
