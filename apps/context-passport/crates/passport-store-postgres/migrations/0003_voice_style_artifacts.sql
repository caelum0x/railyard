-- Voice and style passport kinds, plus provider-minted trained artifacts.

-- 1. Relax the kind CHECK from 0001 to admit the new kinds.
ALTER TABLE passports DROP CONSTRAINT IF EXISTS passports_kind_check;
ALTER TABLE passports ADD CONSTRAINT passports_kind_check
    CHECK (kind IN ('character', 'brand', 'persona', 'voice', 'style'));

-- 2. Trained / enrolled per-provider artifacts (voice clones, avatars, LoRAs)
--    derived from ONE locked version. Artifacts are derived data: registering
--    or revoking one never rewrites the immutable version row.
CREATE TABLE IF NOT EXISTS passport_artifacts (
    id              TEXT PRIMARY KEY,
    passport_id     TEXT NOT NULL REFERENCES passports (id) ON DELETE CASCADE,
    version         BIGINT NOT NULL,
    source_digest   TEXT,
    provider        TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('voice_clone', 'avatar', 'lora')),
    artifact_ref    TEXT,
    provider_model  TEXT,
    status          TEXT NOT NULL CHECK (status IN ('pending', 'ready', 'failed', 'revoked')),
    failure_reason  TEXT,
    created_at      TIMESTAMPTZ NOT NULL,
    updated_at      TIMESTAMPTZ NOT NULL,
    FOREIGN KEY (passport_id, version) REFERENCES passport_versions (passport_id, version) ON DELETE CASCADE,
    CHECK (status <> 'ready' OR artifact_ref IS NOT NULL)
);

-- At most one ACTIVE artifact per (passport, version, provider, kind).
CREATE UNIQUE INDEX IF NOT EXISTS passport_artifacts_one_active
    ON passport_artifacts (passport_id, version, provider, kind)
    WHERE status IN ('pending', 'ready');

CREATE INDEX IF NOT EXISTS passport_artifacts_passport_created
    ON passport_artifacts (passport_id, created_at DESC);
