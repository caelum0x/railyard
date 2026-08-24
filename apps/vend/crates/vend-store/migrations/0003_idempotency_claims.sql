-- Atomic data-plane request claims. A nullable response represents in-progress;
-- completed rows are immutable replays. Existing legacy rows fail closed as a
-- different request hash rather than being replayed without validation.
ALTER TABLE idempotency_keys
    ALTER COLUMN response_body DROP NOT NULL,
    ADD COLUMN IF NOT EXISTS request_hash TEXT NOT NULL DEFAULT 'legacy',
    ADD COLUMN IF NOT EXISTS state TEXT NOT NULL DEFAULT 'complete';

ALTER TABLE idempotency_keys
    ADD CONSTRAINT idempotency_keys_state_check
    CHECK (state IN ('pending', 'complete'));

CREATE INDEX IF NOT EXISTS idempotency_pending_created
    ON idempotency_keys (created_at)
    WHERE state = 'pending';
