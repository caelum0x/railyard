-- 0001_core_accounts.sql — §1 identity tables.
-- All money columns are BIGINT µ$ (never floats).

CREATE TABLE IF NOT EXISTS accounts (
    id          text PRIMARY KEY,
    kind        smallint    NOT NULL,
    owner_ref   text,
    status      smallint    NOT NULL DEFAULT 0,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS account_keys (
    id          text PRIMARY KEY,
    account_id  text        NOT NULL REFERENCES accounts(id),
    pubkey      bytea       NOT NULL,
    custody     smallint    NOT NULL DEFAULT 0,
    revoked_at  timestamptz
);

CREATE TABLE IF NOT EXISTS addresses (
    address     text PRIMARY KEY,
    account_id  text        NOT NULL REFERENCES accounts(id),
    purpose     text,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS addresses_account ON addresses(account_id);
