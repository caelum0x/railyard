-- 0010_payouts_and_reporting.sql — durable post-burn payout log + read-side
-- indexes for statements / solvency.
--
-- A redeem burns AIT atomically and THEN initiates the provider payout. Before
-- this table the payout outcome lived only in the HTTP response, so a failed
-- payout left no durable trace. Every attempt now upserts a row here keyed by
-- the redeem's payout_ref; operators list failed/pending rows and retry them
-- (POST /v1/payouts/:ref/retry) against the already-burned supply.
--
-- Unlike journal/receipts this table is intentionally MUTABLE (status moves
-- pending → paid | failed → paid on retry); the burn itself stays in the
-- append-only journal.

CREATE TABLE IF NOT EXISTS payouts (
    payout_ref     text PRIMARY KEY,
    account_id     text     NOT NULL REFERENCES accounts(id),
    amount_micros  bigint   NOT NULL CHECK (amount_micros > 0),
    dest           jsonb    NOT NULL,
    status         text     NOT NULL CHECK (status IN ('pending', 'paid', 'failed')),
    provider_ref   text,
    attempts       integer  NOT NULL DEFAULT 0,
    last_error     text,
    burn_seq       bigint   NOT NULL REFERENCES journal(seq),
    created_ms     bigint   NOT NULL,
    updated_ms     bigint   NOT NULL
);

CREATE INDEX IF NOT EXISTS payouts_status_created ON payouts (status, created_ms DESC);
CREATE INDEX IF NOT EXISTS payouts_account_created ON payouts (account_id, created_ms DESC);
CREATE INDEX IF NOT EXISTS payouts_created ON payouts (created_ms DESC, payout_ref DESC);

-- Statements filter one account's chain by time.
CREATE INDEX IF NOT EXISTS receipts_account_ts ON receipts (account_id, ts);
