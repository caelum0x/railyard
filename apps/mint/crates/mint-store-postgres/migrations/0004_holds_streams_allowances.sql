-- 0004_holds_streams_allowances.sql — the mutable domain projections.

CREATE TABLE IF NOT EXISTS holds (
    id               text PRIMARY KEY,
    account_id       text     NOT NULL REFERENCES accounts(id),
    amount_micros    bigint   NOT NULL,
    captured_micros  bigint   NOT NULL DEFAULT 0,
    status           smallint NOT NULL,
    ref              text,
    created_at       bigint   NOT NULL,
    closed_at        bigint
);

CREATE TABLE IF NOT EXISTS streams (
    id             text PRIMARY KEY,
    from_id        text     NOT NULL REFERENCES accounts(id),
    to_id          text     NOT NULL REFERENCES accounts(id),
    rate_micros    bigint   NOT NULL,
    unit           smallint NOT NULL,
    cap_micros     bigint   NOT NULL,
    settled_micros bigint   NOT NULL DEFAULT 0,
    last_seq       bigint   NOT NULL DEFAULT 0,
    status         smallint NOT NULL,
    ref            text,
    created_at     bigint   NOT NULL
);

CREATE TABLE IF NOT EXISTS allowances (
    id             text PRIMARY KEY,
    account_id     text     NOT NULL REFERENCES accounts(id),
    key_id         text     NOT NULL,
    limit_micros   bigint   NOT NULL,
    spent_micros   bigint   NOT NULL DEFAULT 0,
    window_kind    smallint NOT NULL,
    window_ms      bigint   NOT NULL DEFAULT 0,
    window_start   bigint   NOT NULL,
    scope          jsonb,
    revoked_at     bigint
);
