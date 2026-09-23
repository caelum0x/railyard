-- 0012_hold_expiry_payer_keys.sql — hold auto-release + AITP payer keys.
--
-- * hold_expiries: optional auto-release deadline per hold. AITP-402 hold
--   settlements (and any hold created with `expires_in_ms`) get one, and the
--   server's background sweeper releases OPEN holds past their deadline so an
--   abandoned hold never locks payer funds forever.
-- * payer_keys: DB-backed AITP-402 payer verifying keys (Ed25519 public
--   halves only), registered / revoked from the API and the operator console
--   instead of a restart-only env var. At most one ACTIVE key per
--   (payer, pubkey_id); a revoked id may be re-registered.
-- * listing indexes for the operator console's per-account holds / streams /
--   allowances tables.

CREATE TABLE IF NOT EXISTS hold_expiries (
    hold_id     text PRIMARY KEY REFERENCES holds(id),
    expires_at  bigint NOT NULL
);

CREATE INDEX IF NOT EXISTS hold_expiries_due ON hold_expiries (expires_at);

CREATE INDEX IF NOT EXISTS holds_account_created ON holds (account_id, created_at DESC);
CREATE INDEX IF NOT EXISTS streams_from_created ON streams (from_id, created_at DESC);
CREATE INDEX IF NOT EXISTS streams_to_created ON streams (to_id, created_at DESC);
CREATE INDEX IF NOT EXISTS allowances_account ON allowances (account_id);

CREATE TABLE IF NOT EXISTS payer_keys (
    payer           text   NOT NULL REFERENCES accounts(id),
    pubkey_id       text   NOT NULL,
    public_key_hex  text   NOT NULL CHECK (length(public_key_hex) = 64),
    label           text,
    created_ms      bigint NOT NULL,
    revoked_ms      bigint,
    PRIMARY KEY (payer, pubkey_id, created_ms)
);

CREATE UNIQUE INDEX IF NOT EXISTS payer_keys_active
    ON payer_keys (payer, pubkey_id) WHERE revoked_ms IS NULL;
CREATE INDEX IF NOT EXISTS payer_keys_created ON payer_keys (created_ms DESC);

GRANT SELECT, INSERT, UPDATE, DELETE ON hold_expiries TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON payer_keys    TO mint_app;
