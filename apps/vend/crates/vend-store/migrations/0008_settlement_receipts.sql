-- Preserve Mint's signed, hash-linked terminal proof beside Vend's product
-- projection. Mint remains the authoritative money ledger; these columns make
-- each Vend charge auditable by the same request/receipt chain.
ALTER TABLE metered_usage
    ADD COLUMN IF NOT EXISTS settlement_seq BIGINT,
    ADD COLUMN IF NOT EXISTS settlement_operation TEXT,
    ADD COLUMN IF NOT EXISTS settlement_op_hash TEXT,
    ADD COLUMN IF NOT EXISTS settlement_receipt_hash TEXT,
    ADD COLUMN IF NOT EXISTS settlement_signature TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS metered_usage_settlement_receipt_hash
    ON metered_usage (settlement_receipt_hash)
    WHERE settlement_receipt_hash IS NOT NULL;
