-- Durable ownership and reconciliation metadata for paid provider batches.
ALTER TABLE relay_billing_outbox
    ADD COLUMN IF NOT EXISTS batch_id UUID,
    ADD COLUMN IF NOT EXISTS billing_model TEXT;

ALTER TABLE relay_billing_outbox
    DROP CONSTRAINT IF EXISTS relay_billing_outbox_state_check;

ALTER TABLE relay_billing_outbox
    ADD CONSTRAINT relay_billing_outbox_state_check CHECK (state IN (
        'reserve_pending', 'held', 'inference_started', 'batch_pending',
        'terminal_pending', 'completed'
    ));

ALTER TABLE relay_billing_outbox
    DROP CONSTRAINT IF EXISTS relay_billing_outbox_batch_metadata_check;

ALTER TABLE relay_billing_outbox
    ADD CONSTRAINT relay_billing_outbox_batch_metadata_check CHECK (
        (batch_id IS NULL) = (billing_model IS NULL)
        AND (state <> 'batch_pending' OR batch_id IS NOT NULL)
    );

CREATE UNIQUE INDEX IF NOT EXISTS relay_billing_outbox_batch_id_idx
    ON relay_billing_outbox (batch_id)
    WHERE batch_id IS NOT NULL;

DROP INDEX IF EXISTS relay_billing_outbox_due_idx;

CREATE INDEX relay_billing_outbox_due_idx
    ON relay_billing_outbox (available_at, lease_until)
    WHERE state NOT IN ('batch_pending', 'completed');
