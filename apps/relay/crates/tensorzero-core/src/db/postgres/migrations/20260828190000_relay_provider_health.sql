-- Restart-durable snapshots for Relay's adaptive provider scorer. This table
-- is an optimization checkpoint only; it is never billing/request authority.
CREATE TABLE IF NOT EXISTS relay_provider_health (
    model_name TEXT NOT NULL,
    provider_name TEXT NOT NULL,
    snapshot JSONB NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (model_name, provider_name),
    CHECK (model_name <> '' AND provider_name <> '')
);
