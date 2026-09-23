-- Per-API-key monthly spend caps for Relay. A cap bounds the month-to-date
-- (UTC calendar month) sum of `relay_cost_events.charged_micros` for one
-- gateway API-key principal; the gateway rejects new inference once the cap is
-- reached, in both Mint-settled billing and postpaid meter-only modes.
CREATE TABLE IF NOT EXISTS relay_key_budgets (
    owner_principal TEXT PRIMARY KEY,
    monthly_limit_micros BIGINT NOT NULL CHECK (monthly_limit_micros > 0),
    note TEXT,
    updated_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
