-- Phase A-1: Token hardening.
-- Adds per-token scopes, optional expiry, and revocation to the tokens table.
-- Existing rows gain empty scopes (full access, legacy behaviour) and null
-- expiry / revoked_at (valid, no expiry), so no data migration is needed.

ALTER TABLE tokens
    ADD COLUMN IF NOT EXISTS scopes      TEXT[]      NOT NULL DEFAULT '{}',
    ADD COLUMN IF NOT EXISTS expires_at  TIMESTAMPTZ,
    ADD COLUMN IF NOT EXISTS revoked_at  TIMESTAMPTZ;

-- Fast expiry sweep and revocation checks at auth time.
CREATE INDEX IF NOT EXISTS tokens_revoked_idx  ON tokens (revoked_at)  WHERE revoked_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS tokens_expires_idx  ON tokens (expires_at)  WHERE expires_at IS NOT NULL;
