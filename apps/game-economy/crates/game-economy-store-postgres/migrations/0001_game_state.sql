-- game-economy durable state (multi-instance safe).
--
-- Every mutation the storefront makes runs in one transaction that row-locks
-- the player's inventory (SELECT ... FOR UPDATE), so concurrent purchases,
-- pack opens, pass advances and refunds on different server instances
-- serialize per player instead of losing updates.

CREATE TABLE IF NOT EXISTS game_inventories (
    player      TEXT PRIMARY KEY,
    inventory   JSONB NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS game_purchases (
    idem_key        TEXT PRIMARY KEY,
    player          TEXT NOT NULL,
    product_sku     TEXT NOT NULL,
    state           TEXT NOT NULL CHECK (state IN ('pending', 'completed')),
    receipt_id      TEXT,
    settlement_seq  BIGINT,
    charged_micros  BIGINT,
    outcome         JSONB,
    reversal_kind   TEXT CHECK (reversal_kind IN ('refunded', 'revoked')),
    reversal        JSONB,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at    TIMESTAMPTZ,
    reversed_at     TIMESTAMPTZ,
    CHECK (state = 'pending' OR (receipt_id IS NOT NULL AND outcome IS NOT NULL))
);

-- One Mint receipt backs exactly one purchase per player.
CREATE UNIQUE INDEX IF NOT EXISTS game_purchases_player_receipt
    ON game_purchases (player, receipt_id) WHERE receipt_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS game_purchases_player_seq
    ON game_purchases (player, settlement_seq DESC);
CREATE INDEX IF NOT EXISTS game_purchases_completed_at
    ON game_purchases (completed_at DESC) WHERE state = 'completed';

-- Single-owner reservations (cosmetics, battle passes). The primary key is the
-- cross-instance guard: two concurrent purchases of the same good with
-- different idempotency keys cannot both insert. A reservation lives from
-- before the Mint hold until the purchase fails (released) or is reversed.
CREATE TABLE IF NOT EXISTS game_reservations (
    player         TEXT NOT NULL,
    ownership_key  TEXT NOT NULL,
    idem_key       TEXT NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (player, ownership_key)
);
