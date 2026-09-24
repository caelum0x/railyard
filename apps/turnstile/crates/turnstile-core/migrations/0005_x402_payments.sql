-- x402 payments accepted by Turnstile (Coinbase/x402 Foundation HTTP 402 wire).
--
-- One row per signed EIP-3009 authorization, keyed by its nonce. The primary
-- key is the replay guard: a replayed X-PAYMENT header cannot reserve a second
-- row, so it can never buy a second protected call on any replica.
--
-- phase:       reserved (verified, handler running) -> settled | abandoned
--              | unknown (facilitator /settle failed in transit; reconcile on-chain)
-- mint_status: credit of the settled USDC into the payee's Mint account
--              (POST /v1/mint with deposit_ref usdc:<tx>, re-verified on-chain)
CREATE TABLE IF NOT EXISTS turnstile_x402_payments (
    nonce          TEXT PRIMARY KEY,
    resource       TEXT NOT NULL,
    network        TEXT NOT NULL,
    payer          TEXT NOT NULL,
    pay_to         TEXT NOT NULL,
    amount_micros  BIGINT NOT NULL CHECK (amount_micros >= 0),
    phase          TEXT NOT NULL CHECK (phase IN ('reserved', 'settled', 'abandoned', 'unknown')),
    transaction    TEXT,
    mint_status    TEXT NOT NULL CHECK (mint_status IN (
        'not_configured', 'pending', 'credited', 'failed'
    )),
    mint_attempts  INTEGER NOT NULL DEFAULT 0,
    last_error     TEXT NOT NULL DEFAULT '',
    created_at_ms  BIGINT NOT NULL,
    updated_at_ms  BIGINT NOT NULL
);

CREATE INDEX IF NOT EXISTS turnstile_x402_payments_pending_credit
    ON turnstile_x402_payments (updated_at_ms)
    WHERE phase = 'settled' AND mint_status = 'pending';

CREATE INDEX IF NOT EXISTS turnstile_x402_payments_recent
    ON turnstile_x402_payments (created_at_ms DESC);

CREATE UNIQUE INDEX IF NOT EXISTS turnstile_x402_payments_tx
    ON turnstile_x402_payments (transaction)
    WHERE transaction IS NOT NULL;
