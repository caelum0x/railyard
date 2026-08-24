-- Bounded, crash-expiring leases for money reconciliation. Multiple one-shot
-- workers may safely claim disjoint rows with FOR UPDATE SKIP LOCKED. A dead
-- worker's lease expires and another worker resumes the idempotent Mint calls.
ALTER TABLE metered_usage
    ADD COLUMN reconcile_lease_token TEXT,
    ADD COLUMN reconcile_lease_until TIMESTAMPTZ,
    ADD COLUMN reconcile_attempts INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN reconcile_last_error TEXT;

CREATE INDEX metered_usage_reconcile_pending
    ON metered_usage (created_at, charge_id)
    WHERE outcome = 'pending';

ALTER TABLE payout_executions
    DROP CONSTRAINT IF EXISTS payout_executions_state_check,
    ADD CONSTRAINT payout_executions_state_check
        CHECK (state IN ('pending', 'complete', 'cancelled')),
    ADD COLUMN reconcile_lease_token TEXT,
    ADD COLUMN reconcile_lease_until TIMESTAMPTZ,
    ADD COLUMN reconcile_attempts INTEGER NOT NULL DEFAULT 0;
