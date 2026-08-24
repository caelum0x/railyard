-- 0006_reserve_deposits.sql — reserve movement log + on-ramp deposits.

CREATE TABLE IF NOT EXISTS reserve_ledger (
    seq          bigserial PRIMARY KEY,
    kind         text   NOT NULL,          -- 'mint' | 'redeem'
    amount_micros bigint NOT NULL,
    source       text,
    journal_seq  bigint REFERENCES journal(seq),
    ts           bigint NOT NULL
);

CREATE TABLE IF NOT EXISTS deposits (
    id                 text PRIMARY KEY,
    source             text   NOT NULL,     -- 'stripe' | 'crypto'
    external_ref       text   NOT NULL UNIQUE,
    amount_micros      bigint NOT NULL,
    fx_note            text,
    account_id         text   REFERENCES accounts(id),
    minted_journal_seq bigint REFERENCES journal(seq),
    status             smallint NOT NULL DEFAULT 0
);
