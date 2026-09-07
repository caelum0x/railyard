-- Turnstile's shared single-use nonce set (SPEC §5.1), moved out of process so a
-- horizontally-scaled Turnstile fleet enforces replay protection against ONE
-- store. A nonce issued by any replica is registered here; the first replica to
-- consume it wins the row (atomic DELETE ... RETURNING) and the rest see a replay.
--
-- This is NOT a money ledger: it holds only short-lived challenge ownership and
-- expiry. Rows are consumed on use and swept once expired (the reaper).
CREATE TABLE IF NOT EXISTS turnstile_nonces (
    nonce      TEXT PRIMARY KEY,
    expiry_ms  BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- The reaper deletes by expiry; index it so sweeps stay cheap as the fleet scales.
CREATE INDEX IF NOT EXISTS turnstile_nonces_expiry ON turnstile_nonces (expiry_ms);
