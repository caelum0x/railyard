-- Restart-durable Relay -> Mint reservation and terminal settlement state.
-- One row owns the complete lifecycle so capture-vs-release is selected once.
CREATE TABLE IF NOT EXISTS relay_billing_outbox (
    owner_principal TEXT NOT NULL,
    payer_account TEXT NOT NULL,
    payee_account TEXT,
    request_id UUID NOT NULL,
    estimate_micros BIGINT NOT NULL CHECK (estimate_micros >= 0),
    held_amount_micros BIGINT NOT NULL CHECK (
        held_amount_micros >= estimate_micros
    ),
    reserve_idem_key TEXT NOT NULL UNIQUE,
    reserve_payload JSONB NOT NULL,
    hold_id TEXT,
    state TEXT NOT NULL CHECK (state IN (
        'reserve_pending', 'held', 'inference_started',
        'terminal_pending', 'completed'
    )),
    terminal_operation TEXT CHECK (terminal_operation IN ('capture', 'release')),
    terminal_idem_key TEXT UNIQUE,
    terminal_payload JSONB,
    receipt JSONB,
    attestation JSONB,
    attempt_count INTEGER NOT NULL DEFAULT 0,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    lease_owner UUID,
    lease_until TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    PRIMARY KEY (owner_principal, request_id),
    CHECK ((terminal_operation IS NULL) = (terminal_payload IS NULL)),
    CHECK ((terminal_operation IS NULL) = (terminal_idem_key IS NULL))
);

CREATE INDEX IF NOT EXISTS relay_billing_outbox_due_idx
    ON relay_billing_outbox (available_at, lease_until)
    WHERE state <> 'completed';

CREATE INDEX IF NOT EXISTS relay_billing_outbox_payer_request_idx
    ON relay_billing_outbox (payer_account, request_id);

ALTER TABLE relay_settlement_evidence
    ADD COLUMN IF NOT EXISTS attestation JSONB,
    ADD COLUMN IF NOT EXISTS verified_at_ms BIGINT;
