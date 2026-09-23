-- Runtime-editable catalog.
--
-- GAME_CATALOG_PATH seeds these tables on first boot; afterwards the operator
-- console edits them live. `game_catalog_meta.revision` is the optimistic
-- concurrency token: every save locks the single meta row, checks the revision
-- the edit was based on, rewrites products/pools and bumps it, so two
-- operators (or two server instances) can never silently overwrite each other.
--
-- Products are never hard-deleted: archiving keeps the definition resolvable
-- for packs, passes and cosmetics players already own.

CREATE TABLE IF NOT EXISTS game_catalog_meta (
    id          SMALLINT PRIMARY KEY CHECK (id = 1),
    revision    BIGINT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS game_catalog_products (
    sku         TEXT PRIMARY KEY,
    kind        TEXT NOT NULL CHECK (kind IN ('card_pack', 'cosmetic', 'battle_pass')),
    product     JSONB NOT NULL,
    archived    BOOLEAN NOT NULL DEFAULT false,
    position    INTEGER NOT NULL DEFAULT 0,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS game_catalog_pools (
    rarity      TEXT PRIMARY KEY
                CHECK (rarity IN ('Common', 'Uncommon', 'Rare', 'Epic', 'Legendary')),
    items       JSONB NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
