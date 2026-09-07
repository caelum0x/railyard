-- Phase 2: authentication + repository visibility.
-- Personal access tokens (PATs) authenticate git-over-HTTP: only the SHA-256
-- hash of a token is stored, never the token itself. Repository visibility
-- gates anonymous clone (public) vs. authenticated-member-only (private).

ALTER TABLE repositories
    ADD COLUMN IF NOT EXISTS visibility TEXT NOT NULL DEFAULT 'public';

CREATE TABLE IF NOT EXISTS tokens (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    token_hash  TEXT NOT NULL UNIQUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS tokens_user_id_idx ON tokens (user_id);
