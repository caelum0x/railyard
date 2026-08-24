-- Character Lock Passport storage. Three record kinds: passport heads, immutable
-- versions, and generation provenance. Asset fields are stored as typed JSONB so
-- the character/brand/persona shape evolves without a migration per field.

CREATE TABLE IF NOT EXISTS passports (
    id           TEXT PRIMARY KEY,
    kind         TEXT NOT NULL CHECK (kind IN ('character', 'brand', 'persona')),
    owner        TEXT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('draft', 'locked')),
    version      BIGINT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS passports_owner_created
    ON passports (owner, created_at DESC);
CREATE INDEX IF NOT EXISTS passports_kind_created
    ON passports (kind, created_at DESC);

-- One immutable-once-locked snapshot per (passport, version). A locked row must
-- never be rewritten; the service enforces this and only ever inserts new
-- versions or updates still-draft rows.
CREATE TABLE IF NOT EXISTS passport_versions (
    passport_id  TEXT NOT NULL REFERENCES passports (id) ON DELETE CASCADE,
    version      BIGINT NOT NULL,
    status       TEXT NOT NULL CHECK (status IN ('draft', 'locked')),
    assets       JSONB NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL,
    updated_at   TIMESTAMPTZ NOT NULL,
    locked_at    TIMESTAMPTZ,
    PRIMARY KEY (passport_id, version),
    CHECK ((status = 'locked') = (locked_at IS NOT NULL))
);

-- Idempotency ledger for create: one key maps to exactly one passport.
CREATE TABLE IF NOT EXISTS passport_idempotency (
    idempotency_key  TEXT PRIMARY KEY,
    passport_id      TEXT NOT NULL REFERENCES passports (id) ON DELETE CASCADE
);

-- Provenance: which passport + locked version conditioned a generated asset.
CREATE TABLE IF NOT EXISTS passport_generations (
    id           TEXT PRIMARY KEY,
    passport_id  TEXT NOT NULL REFERENCES passports (id) ON DELETE CASCADE,
    version      BIGINT NOT NULL,
    provider     TEXT NOT NULL,
    modality     TEXT NOT NULL,
    output_ref   TEXT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL
);

CREATE INDEX IF NOT EXISTS passport_generations_passport_created
    ON passport_generations (passport_id, created_at DESC);
