-- Vend's OWN persistence (Neon Postgres). Stores tenancy / policy / usage and
-- the hold_id <-> job mapping. This is NEVER a money ledger — settlement and the
-- authoritative balances live on Mint.

CREATE TABLE IF NOT EXISTS tenants (
    id              TEXT PRIMARY KEY,
    name            TEXT NOT NULL,
    mint_account_id TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS apps (
    id              TEXT PRIMARY KEY,
    tenant_id       TEXT NOT NULL REFERENCES tenants(id),
    name            TEXT NOT NULL,
    model_allowlist JSONB NOT NULL DEFAULT '[]'::jsonb,
    zdr             BOOLEAN NOT NULL DEFAULT false,
    mint_account_id TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS markup_policy (
    app_id            TEXT PRIMARY KEY REFERENCES apps(id),
    markup_bps        INTEGER NOT NULL,
    min_fee_micros    BIGINT NOT NULL,
    platform_fee_bps  INTEGER NOT NULL,
    funding_mode      TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS end_users (
    id              TEXT PRIMARY KEY,
    app_id          TEXT NOT NULL REFERENCES apps(id),
    external_ref    TEXT NOT NULL,
    mint_account_id TEXT,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (app_id, external_ref)
);

CREATE TABLE IF NOT EXISTS sub_keys (
    id                            TEXT PRIMARY KEY,
    tenant_id                     TEXT NOT NULL REFERENCES tenants(id),
    app_id                        TEXT REFERENCES apps(id),
    end_user_id                   TEXT REFERENCES end_users(id),
    scope                         SMALLINT NOT NULL,
    status                        TEXT NOT NULL,
    prefix                        TEXT NOT NULL UNIQUE,
    secret_hash                   TEXT NOT NULL,
    daily_cap_micros              BIGINT,
    per_end_user_daily_cap_micros BIGINT,
    created_at                    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS metered_usage (
    charge_id            TEXT PRIMARY KEY,
    sub_key_id           TEXT NOT NULL,
    tenant_id            TEXT NOT NULL,
    app_id               TEXT NOT NULL,
    end_user_id          TEXT,
    idem_key             TEXT NOT NULL,
    hold_id              TEXT NOT NULL,
    provider_cost_micros BIGINT NOT NULL,
    charge_micros        BIGINT NOT NULL,
    platform_fee_micros  BIGINT NOT NULL,
    tenant_net_micros    BIGINT NOT NULL,
    outcome              TEXT NOT NULL,
    utc_day              TEXT NOT NULL,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS metered_usage_key_day
    ON metered_usage (sub_key_id, utc_day);
CREATE INDEX IF NOT EXISTS metered_usage_end_user_day
    ON metered_usage (end_user_id, utc_day);

-- Local mirror of Mint's reserve snapshot (cache only; Mint is authoritative).
CREATE TABLE IF NOT EXISTS tenant_reserve (
    tenant_id       TEXT PRIMARY KEY REFERENCES tenants(id),
    balance_micros  BIGINT NOT NULL DEFAULT 0,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS tenant_reserve_ledger (
    id            BIGSERIAL PRIMARY KEY,
    tenant_id     TEXT NOT NULL REFERENCES tenants(id),
    delta_micros  BIGINT NOT NULL,
    reason        TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS idempotency_keys (
    key           TEXT PRIMARY KEY,
    response_body TEXT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
