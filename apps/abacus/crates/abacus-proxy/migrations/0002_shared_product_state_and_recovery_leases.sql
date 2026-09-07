-- Policy and receipt projections must be visible to every replica. Mint remains
-- authoritative for balances; these are Abacus's transactional read models.
CREATE TABLE IF NOT EXISTS abacus_agent_policies (
    agent_id      TEXT PRIMARY KEY,
    policy_json   JSONB NOT NULL,
    updated_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS abacus_receipts (
    hold_id       TEXT PRIMARY KEY,
    request_id    TEXT NOT NULL UNIQUE,
    receipt_json  JSONB NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

ALTER TABLE abacus_open_holds
    ADD COLUMN IF NOT EXISTS lease_owner TEXT,
    ADD COLUMN IF NOT EXISTS lease_until_ms BIGINT;

CREATE INDEX IF NOT EXISTS abacus_open_holds_recovery
    ON abacus_open_holds (lease_until_ms, created_at_ms, hold_id);
