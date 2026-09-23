-- 0011_operator_plane.sql — operator control plane.
--
-- * accounts.status is now live: 0 = active, 1 = frozen, 2 = closed.
-- * api_keys: DB-backed, hashed, optionally account-scoped service keys
--   (managed from the console / POST /v1/keys; the secret is never stored).
-- * provider_reports / provider_report_lines: imported provider settlement
--   reports (Stripe balance transactions, PayPal settlement, on-chain transfer
--   lists …) matched against Mint's reserve movements and payouts.
-- * deposit_reversals: refunds / chargebacks of already-minted deposits and
--   the claw-back burn state (operator worklist for shortfalls).
-- * payouts gain a terminal 'cancelled' state (burn re-credited).
--
-- None of these tables are money: journal / receipts / reserve_ledger remain
-- the append-only source of truth.

CREATE INDEX IF NOT EXISTS accounts_status ON accounts (status) WHERE status <> 0;

CREATE TABLE IF NOT EXISTS api_keys (
    id          text PRIMARY KEY,
    name        text    NOT NULL,
    key_hash    text    NOT NULL UNIQUE,
    prefix      text    NOT NULL,
    scopes      jsonb   NOT NULL,
    accounts    jsonb,
    created_ms  bigint  NOT NULL,
    revoked_ms  bigint
);

CREATE INDEX IF NOT EXISTS api_keys_created ON api_keys (created_ms DESC);

CREATE TABLE IF NOT EXISTS provider_reports (
    id              text PRIMARY KEY,
    provider        text    NOT NULL,
    label           text,
    imported_ms     bigint  NOT NULL,
    line_count      bigint  NOT NULL,
    period_start_ms bigint,
    period_end_ms   bigint
);

CREATE INDEX IF NOT EXISTS provider_reports_imported ON provider_reports (imported_ms DESC);

CREATE TABLE IF NOT EXISTS provider_report_lines (
    report_id     text    NOT NULL REFERENCES provider_reports(id) ON DELETE CASCADE,
    line_no       integer NOT NULL,
    kind          text    NOT NULL CHECK (kind IN ('deposit', 'payout', 'refund', 'dispute', 'other')),
    reference     text    NOT NULL,
    amount_micros bigint  NOT NULL,
    currency      text,
    occurred_ms   bigint,
    PRIMARY KEY (report_id, line_no)
);

CREATE TABLE IF NOT EXISTS deposit_reversals (
    deposit_ref    text PRIMARY KEY,
    account_id     text    NOT NULL REFERENCES accounts(id),
    amount_micros  bigint  NOT NULL CHECK (amount_micros > 0),
    clawed_micros  bigint  NOT NULL DEFAULT 0 CHECK (clawed_micros >= 0),
    reason         text    NOT NULL,
    status         text    NOT NULL CHECK (status IN ('clawed_back', 'shortfall', 'written_off')),
    attempts       integer NOT NULL DEFAULT 0,
    note           text,
    created_ms     bigint  NOT NULL,
    updated_ms     bigint  NOT NULL
);

CREATE INDEX IF NOT EXISTS deposit_reversals_status ON deposit_reversals (status, created_ms DESC);

ALTER TABLE payouts DROP CONSTRAINT IF EXISTS payouts_status_check;
ALTER TABLE payouts ADD CONSTRAINT payouts_status_check
    CHECK (status IN ('pending', 'paid', 'failed', 'cancelled'));

-- Least-privilege app role: operator tables are mutable.
GRANT SELECT, INSERT, UPDATE, DELETE ON payouts               TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON api_keys              TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON provider_reports      TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON provider_report_lines TO mint_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON deposit_reversals     TO mint_app;
