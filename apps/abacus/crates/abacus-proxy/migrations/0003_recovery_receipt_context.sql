-- Exact request/principal context is captured before forwarding so a recovery
-- worker can write the same signed released receipt as the normal terminal path.
ALTER TABLE abacus_open_holds
    ADD COLUMN IF NOT EXISTS receipt_context JSONB;
