-- 0005_receipts.sql — the per-account hash-linked, signed receipt chain (§6).

CREATE TABLE IF NOT EXISTS receipts (
    account_id            text   NOT NULL REFERENCES accounts(id),
    seq                   bigint NOT NULL,          -- per-account chain position
    journal_seq           bigint NOT NULL REFERENCES journal(seq),
    op                    text   NOT NULL,
    op_hash               bytea  NOT NULL,
    amount_micros         bigint NOT NULL,
    from_id               text,
    to_id                 text,
    balance_after_micros  bigint NOT NULL,          -- available leg
    balance_after_held    bigint NOT NULL DEFAULT 0,
    ts                    bigint NOT NULL,
    prev_hash             bytea  NOT NULL,
    hash                  bytea  NOT NULL,
    sig                   bytea  NOT NULL,
    signer                text   NOT NULL,
    global_seq            bigserial,                -- global append order
    PRIMARY KEY (account_id, seq)
);

CREATE INDEX IF NOT EXISTS receipts_global ON receipts(global_seq);
