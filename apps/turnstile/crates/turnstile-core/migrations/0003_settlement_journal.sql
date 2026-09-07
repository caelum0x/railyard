CREATE TABLE IF NOT EXISTS turnstile_settlements (
    nonce               TEXT PRIMARY KEY,
    authorization_json  TEXT NOT NULL,
    phase               TEXT NOT NULL CHECK (phase IN (
        'verifying', 'held', 'capture_pending', 'captured',
        'release_pending', 'released', 'transfer_complete', 'rejected'
    )),
    hold_id             TEXT,
    scheme_json         TEXT,
    actual_micros       BIGINT,
    receipt_json        TEXT,
    owner_token         TEXT NOT NULL,
    lease_until_ms      BIGINT NOT NULL,
    last_error          TEXT NOT NULL DEFAULT '',
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS turnstile_settlements_recovery_idx
    ON turnstile_settlements (lease_until_ms, nonce)
    WHERE phase NOT IN ('captured', 'released', 'transfer_complete', 'rejected');
