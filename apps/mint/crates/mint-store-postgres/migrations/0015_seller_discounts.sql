-- Seller discount ownership (Wave Q, for Menivor QM1.3 creator coupons).
-- Mint makes a discount code on the seller's own processor account; the terms
-- live there. Only which seller of which calling product made which
-- processor discount id is kept, so one seller can never stop or change
-- another seller's code. Beside the ledger and never in it. Idempotent: safe
-- if applied by hand before sqlx records it.
CREATE TABLE IF NOT EXISTS mint_seller_discounts (
    product       TEXT NOT NULL,
    seller        TEXT NOT NULL,
    discount_id   TEXT NOT NULL CHECK (discount_id ~ '^[A-Za-z0-9_]{1,64}$'),
    code          TEXT NOT NULL CHECK (code ~ '^[A-Z0-9_-]{3,32}$'),
    created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (product, seller, discount_id),
    FOREIGN KEY (product, seller) REFERENCES mint_seller_accounts (product, seller)
);
CREATE INDEX IF NOT EXISTS mint_seller_discounts_code_idx ON mint_seller_discounts (product, seller, code, created_at_ms DESC);
