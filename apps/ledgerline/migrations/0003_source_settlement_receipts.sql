-- Durable correlation to Relay/Mint's signed source settlement. Mint remains
-- authoritative; Ledgerline stores the exact proof used to accept the event.
ALTER TABLE margin_events
    ADD COLUMN IF NOT EXISTS source_settlement_receipt TEXT;

CREATE UNIQUE INDEX IF NOT EXISTS margin_events_source_receipt_hash
    ON margin_events ((source_settlement_receipt::jsonb ->> 'receipt_hash'))
    WHERE source_settlement_receipt IS NOT NULL;
