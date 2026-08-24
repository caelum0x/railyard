-- Durable external-operation projection for split payouts. Mint remains the
-- authoritative money ledger. A row is staged before capture, then each
-- idempotent transfer leg is marked as its Mint receipt is observed.
CREATE TABLE payout_executions (
    charge_id                  TEXT PRIMARY KEY REFERENCES metered_usage(charge_id),
    idem_key                   TEXT NOT NULL UNIQUE,
    clearing_account_id        TEXT NOT NULL,
    provider_account_id        TEXT NOT NULL,
    tenant_account_id          TEXT NOT NULL,
    platform_account_id        TEXT NOT NULL,
    provider_cost_micros       BIGINT NOT NULL CHECK (provider_cost_micros >= 0),
    tenant_net_micros          BIGINT NOT NULL CHECK (tenant_net_micros >= 0),
    platform_fee_micros        BIGINT NOT NULL CHECK (platform_fee_micros >= 0),
    captured_charge_micros     BIGINT NOT NULL CHECK (captured_charge_micros >= 0),
    provider_transferred       BOOLEAN NOT NULL DEFAULT false,
    tenant_transferred         BOOLEAN NOT NULL DEFAULT false,
    platform_transferred       BOOLEAN NOT NULL DEFAULT false,
    state                      TEXT NOT NULL DEFAULT 'pending'
                               CHECK (state IN ('pending', 'complete')),
    last_error                 TEXT,
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at                 TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (
        captured_charge_micros =
        provider_cost_micros + tenant_net_micros + platform_fee_micros
    )
);

CREATE INDEX payout_executions_pending
    ON payout_executions (updated_at, charge_id)
    WHERE state = 'pending';
