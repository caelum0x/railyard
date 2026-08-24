-- 0002_journal.sql — the append-only source of truth.
-- Each op appends >= 1 rows whose signed µ$ deltas sum to zero across buckets.
-- Rows belonging to ONE op share op_hash + idem_key, so the UNIQUE exactly-once
-- guard lives on a dedicated `idempotency` table (one row per op), not on the
-- journal (whose rows intentionally repeat idem_key).

CREATE TABLE IF NOT EXISTS journal (
    seq          bigserial PRIMARY KEY,
    op           text     NOT NULL,
    op_hash      bytea    NOT NULL,
    idem_key     text     NOT NULL,
    account_id   text     NOT NULL REFERENCES accounts(id),
    counter_id   text              REFERENCES accounts(id),
    delta_micros bigint   NOT NULL,
    bucket       smallint NOT NULL DEFAULT 0,
    ref          text,
    ts           bigint   NOT NULL
);

CREATE INDEX IF NOT EXISTS journal_idem ON journal(idem_key);
CREATE INDEX IF NOT EXISTS journal_acct ON journal(account_id, seq);

-- One row per committed op. The UNIQUE(idem_key) is THE exactly-once seam:
-- concurrent duplicate idem_keys race here and exactly one wins the INSERT.
CREATE TABLE IF NOT EXISTS idempotency (
    idem_key   text PRIMARY KEY,
    op_hash    bytea  NOT NULL,
    first_seq  bigint NOT NULL DEFAULT 0,  -- first journal seq (0 for control-plane ops)
    result_ref text,                       -- minted hold/stream/allowance id, when any
    kind       text   NOT NULL,
    created_at bigint NOT NULL
);
