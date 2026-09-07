-- Crash-visible progress for the Mint hold/capture/COGS settlement saga.
CREATE TABLE IF NOT EXISTS settlement_outbox (
    request_id     TEXT PRIMARY KEY,
    event_json     TEXT NOT NULL,
    attempt        BIGINT NOT NULL CHECK (attempt > 0 AND attempt <= 4294967295),
    state          TEXT NOT NULL CHECK (state IN ('pending', 'settled', 'compensating', 'compensated')),
    hold_state     TEXT NOT NULL CHECK (hold_state IN ('pending', 'committed', 'compensated')),
    hold_id        TEXT,
    hold_seq       BIGINT,
    capture_state  TEXT NOT NULL CHECK (capture_state IN ('pending', 'committed', 'compensated')),
    revenue_seq    BIGINT,
    cogs_state     TEXT NOT NULL CHECK (cogs_state IN ('pending', 'committed', 'compensated')),
    cogs_seq       BIGINT,
    updated_at     TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS settlement_outbox_recovery
    ON settlement_outbox (state, updated_at)
    WHERE state IN ('pending', 'compensating');
