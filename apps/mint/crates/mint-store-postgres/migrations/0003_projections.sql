-- 0003_projections.sql — the rebuildable balance projection (µ$, never floats).

CREATE TABLE IF NOT EXISTS balances (
    account_id       text PRIMARY KEY REFERENCES accounts(id),
    available_micros bigint NOT NULL DEFAULT 0,
    held_micros      bigint NOT NULL DEFAULT 0,
    updated_seq      bigint NOT NULL DEFAULT 0
);

-- The single-row in-store reserve total (§6 supply). id is pinned to 0 so there
-- is exactly one reserve row; mint/redeem mutate it in the same tx as the leg.
CREATE TABLE IF NOT EXISTS reserve (
    id            smallint PRIMARY KEY DEFAULT 0,
    total_micros  bigint   NOT NULL DEFAULT 0
);

INSERT INTO reserve (id, total_micros) VALUES (0, 0)
    ON CONFLICT (id) DO NOTHING;
