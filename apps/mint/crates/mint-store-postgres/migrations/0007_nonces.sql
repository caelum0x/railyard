-- 0007_nonces.sql — single-use AITP-402 challenge nonces (§5.4 replay guard).

CREATE TABLE IF NOT EXISTS nonces (
    account_id text        NOT NULL,
    nonce      text        NOT NULL,
    expiry     bigint      NOT NULL,   -- unix millis
    PRIMARY KEY (account_id, nonce)
);

CREATE INDEX IF NOT EXISTS nonces_expiry ON nonces(expiry);
