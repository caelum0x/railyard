-- White-label kit ("your own OpenRouter"): per-model markup overrides per app.
-- A row replaces the app's markup_bps (and optionally min_fee_micros) for one
-- Relay model id; the platform fee and funding mode stay app-wide.
CREATE TABLE IF NOT EXISTS app_model_markups (
    app_id          TEXT NOT NULL REFERENCES apps(id),
    model           TEXT NOT NULL CHECK (length(model) BETWEEN 1 AND 256),
    markup_bps      INTEGER NOT NULL CHECK (markup_bps BETWEEN 0 AND 1000000),
    min_fee_micros  BIGINT CHECK (min_fee_micros IS NULL OR min_fee_micros >= 0),
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (app_id, model)
);

-- Tenant earnings roll-ups read captured usage by tenant and day.
CREATE INDEX IF NOT EXISTS metered_usage_tenant_day
    ON metered_usage (tenant_id, utc_day)
    WHERE outcome = 'captured';

-- Wallet listing: newest end users first per app.
CREATE INDEX IF NOT EXISTS end_users_app_created
    ON end_users (app_id, created_at DESC);

-- Payout visibility: newest first.
CREATE INDEX IF NOT EXISTS payout_executions_created
    ON payout_executions (created_at DESC);
