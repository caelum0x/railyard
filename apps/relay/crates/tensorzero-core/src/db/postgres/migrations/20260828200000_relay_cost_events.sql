-- Durable Relay cost-event outbox. Rows are inserted in the same transaction
-- that commits Mint settlement evidence and the completed billing command.
CREATE TABLE IF NOT EXISTS relay_cost_events (
    event_id UUID PRIMARY KEY,
    request_id UUID NOT NULL,
    owner_principal TEXT NOT NULL,
    event JSONB NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    next_attempt_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    lease_owner TEXT,
    lease_until TIMESTAMPTZ,
    published_at TIMESTAMPTZ,
    dead_lettered_at TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (owner_principal, request_id)
);

CREATE INDEX IF NOT EXISTS relay_cost_events_due_idx
    ON relay_cost_events (next_attempt_at, lease_until)
    WHERE published_at IS NULL AND dead_lettered_at IS NULL;

CREATE INDEX IF NOT EXISTS relay_cost_events_principal_idx
    ON relay_cost_events (owner_principal, created_at DESC);
