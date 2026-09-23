-- Operator retail price edits for Relay catalog models. Rows overlay the
-- deployed catalog (config/catalog.json or the embedded list prices) at
-- startup and on every gateway replica's periodic refresh, so a price change
-- made in the Models page reaches billing and metering without a redeploy.
CREATE TABLE IF NOT EXISTS relay_catalog_price_overrides (
    model_id TEXT PRIMARY KEY,
    input_micro_usd_per_million BIGINT NOT NULL CHECK (input_micro_usd_per_million >= 0),
    output_micro_usd_per_million BIGINT NOT NULL CHECK (output_micro_usd_per_million >= 0),
    note TEXT,
    updated_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
