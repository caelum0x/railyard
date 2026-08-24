-- Ledgerline owns event metadata and margin projections. Mint remains the only
-- authoritative money ledger.
CREATE TABLE IF NOT EXISTS margin_events (
    idempotency_key      TEXT PRIMARY KEY,
    request_id           TEXT NOT NULL UNIQUE,
    customer_id          TEXT NOT NULL,
    feature              TEXT NOT NULL,
    model                TEXT NOT NULL,
    source               TEXT NOT NULL,
    outcome              TEXT NOT NULL,
    revenue_micros       BIGINT NOT NULL,
    cogs_micros          BIGINT NOT NULL,
    margin_micros        BIGINT NOT NULL,
    margin_bps           BIGINT,
    customer_account     TEXT NOT NULL,
    revenue_account      TEXT NOT NULL,
    cogs_account         TEXT NOT NULL,
    provider_account     TEXT NOT NULL,
    revenue_hold_id      TEXT,
    revenue_sequence     BIGINT,
    cogs_sequence        BIGINT,
    created_at           TIMESTAMPTZ NOT NULL,
    CHECK (revenue_micros >= 0),
    CHECK (cogs_micros >= 0),
    CHECK (
        (outcome = 'succeeded' AND revenue_micros > 0 AND cogs_micros > 0)
        OR (outcome = 'failed' AND revenue_micros = 0 AND cogs_micros = 0)
    )
);

CREATE INDEX IF NOT EXISTS margin_events_customer_created
    ON margin_events (customer_id, created_at DESC);
CREATE INDEX IF NOT EXISTS margin_events_feature_created
    ON margin_events (feature, created_at DESC);
CREATE INDEX IF NOT EXISTS margin_events_model_created
    ON margin_events (model, created_at DESC);

