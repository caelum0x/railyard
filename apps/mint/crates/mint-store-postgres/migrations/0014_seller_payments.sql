-- Seller-own payments (Wave P PY2.3): sellers of a calling product (their
-- credentials sealed with MINT_SELLER_SEAL_KEY), their products, one-time
-- connect tokens, and the entitlement events forwarded to the calling product
-- with an outbox. Beside the ledger and never in it: no balance, no hold, no
-- payout. Idempotent: safe if applied by hand before sqlx records it.
CREATE TABLE IF NOT EXISTS mint_seller_accounts (
    product               TEXT NOT NULL CHECK (product ~ '^[a-z][a-z0-9-]{1,31}$'),
    seller                TEXT NOT NULL CHECK (seller ~ '^[A-Za-z0-9][A-Za-z0-9._-]{0,119}$'),
    provider              TEXT NOT NULL CHECK (provider IN ('dodo', 'stripe')),
    mode                  TEXT NOT NULL CHECK (mode IN ('test', 'live')),
    sealed_secret         TEXT NOT NULL,
    sealed_webhook_secret TEXT,
    account_ref           TEXT CHECK (account_ref IS NULL OR account_ref ~ '^acct_[A-Za-z0-9_]{1,59}$'),
    connected_at_ms       BIGINT NOT NULL,
    PRIMARY KEY (product, seller)
);
CREATE UNIQUE INDEX IF NOT EXISTS mint_seller_accounts_account_idx ON mint_seller_accounts (provider, account_ref) WHERE account_ref IS NOT NULL;

CREATE TABLE IF NOT EXISTS mint_seller_products (
    product       TEXT NOT NULL,
    seller        TEXT NOT NULL,
    sku           TEXT NOT NULL CHECK (sku ~ '^[a-z0-9._-]{1,64}$'),
    name          TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 120),
    price_cents   BIGINT NOT NULL CHECK (price_cents BETWEEN 50 AND 100000000),
    currency      TEXT NOT NULL CHECK (currency ~ '^[A-Z]{3}$'),
    interval      TEXT CHECK (interval IS NULL OR interval IN ('month', 'year')),
    provider_ref  TEXT NOT NULL CHECK (length(provider_ref) BETWEEN 1 AND 200),
    created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (product, seller, sku),
    FOREIGN KEY (product, seller) REFERENCES mint_seller_accounts (product, seller)
);

CREATE TABLE IF NOT EXISTS mint_seller_connect_tokens (
    token_hash    TEXT PRIMARY KEY CHECK (token_hash ~ '^[0-9a-f]{64}$'),
    product       TEXT NOT NULL,
    seller        TEXT NOT NULL,
    return_url    TEXT CHECK (return_url IS NULL OR return_url ~ '^https://'),
    expires_at_ms BIGINT NOT NULL,
    used_at_ms    BIGINT
);

CREATE TABLE IF NOT EXISTS mint_seller_entitlements (
    id                TEXT PRIMARY KEY,
    product           TEXT NOT NULL,
    seller            TEXT NOT NULL,
    provider          TEXT NOT NULL CHECK (provider IN ('dodo', 'stripe')),
    provider_event_id TEXT NOT NULL CHECK (length(provider_event_id) BETWEEN 1 AND 200),
    kind              TEXT NOT NULL CHECK (kind IN ('granted', 'revoked')),
    reason            TEXT CHECK (reason IS NULL OR reason IN ('refund', 'dispute', 'cancelled', 'expired')),
    buyer_ref         TEXT NOT NULL CHECK (length(buyer_ref) BETWEEN 1 AND 200),
    sku               TEXT NOT NULL,
    occurred_at_ms    BIGINT NOT NULL,
    expires_at_ms     BIGINT,
    recorded_at_ms    BIGINT NOT NULL,
    attempts          INTEGER NOT NULL DEFAULT 0,
    next_attempt_ms   BIGINT NOT NULL,
    delivered_ms      BIGINT,
    last_error        TEXT,
    UNIQUE (provider, provider_event_id)
);
CREATE INDEX IF NOT EXISTS mint_seller_entitlements_due_idx ON mint_seller_entitlements (next_attempt_ms) WHERE delivered_ms IS NULL;
CREATE INDEX IF NOT EXISTS mint_seller_entitlements_seller_idx ON mint_seller_entitlements (product, seller, recorded_at_ms DESC);
