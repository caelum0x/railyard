-- Portfolio mirror (Wave P PY1.5): one record per paid event a product
-- received from its payment provider, beside the ledger and never in it (no
-- balance, no hold, no payout). Plus the outbound delivery outbox.
-- Idempotent: safe if applied by hand before sqlx records it.
CREATE TABLE IF NOT EXISTS mint_mirror_events (
    id                TEXT PRIMARY KEY,
    product           TEXT NOT NULL CHECK (product ~ '^[a-z][a-z0-9-]{1,31}$'),
    provider          TEXT NOT NULL CHECK (provider ~ '^[a-z][a-z0-9-]{1,31}$'),
    provider_event_id TEXT NOT NULL CHECK (length(provider_event_id) BETWEEN 1 AND 200),
    kind              TEXT NOT NULL CHECK (kind IN ('payment', 'refund', 'dispute')),
    amount_micros     BIGINT NOT NULL,
    currency          TEXT NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    customer_ref      TEXT NOT NULL CHECK (length(customer_ref) BETWEEN 1 AND 200),
    occurred_at_ms    BIGINT NOT NULL,
    recorded_at_ms    BIGINT NOT NULL,
    UNIQUE (provider, provider_event_id)
);
CREATE INDEX IF NOT EXISTS mint_mirror_events_product_idx ON mint_mirror_events (product, recorded_at_ms DESC);

CREATE TABLE IF NOT EXISTS mint_mirror_deliveries (
    event_id        TEXT NOT NULL REFERENCES mint_mirror_events (id),
    endpoint        TEXT NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    next_attempt_ms BIGINT NOT NULL,
    delivered_ms    BIGINT,
    last_error      TEXT,
    PRIMARY KEY (event_id, endpoint)
);
CREATE INDEX IF NOT EXISTS mint_mirror_deliveries_due_idx ON mint_mirror_deliveries (next_attempt_ms) WHERE delivered_ms IS NULL;
