-- Principal-bound AITP challenge lifecycle. Mint remains authoritative for
-- signature verification and nonce consumption; Relay owns request correlation.
CREATE TABLE IF NOT EXISTS relay_aitp_challenges (
    request_id UUID PRIMARY KEY,
    owner_principal TEXT NOT NULL,
    challenge JSONB NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('issued','authorized','consumed','expired')),
    operation_hash TEXT,
    receipt_hash TEXT,
    issued_at_ms BIGINT NOT NULL,
    updated_at_ms BIGINT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    CHECK ((state IN ('authorized','consumed')) = (operation_hash IS NOT NULL)),
    CHECK ((state = 'consumed') = (receipt_hash IS NOT NULL))
);

CREATE INDEX IF NOT EXISTS relay_aitp_challenges_owner_idx
    ON relay_aitp_challenges (owner_principal, issued_at_ms DESC);

CREATE INDEX IF NOT EXISTS relay_aitp_challenges_expiry_idx
    ON relay_aitp_challenges (expires_at)
    WHERE state IN ('issued','authorized');
