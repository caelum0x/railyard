-- OAuth2 / SSO external identities.
--
-- Links a forge user to an account at an external identity provider (GitHub,
-- Google, ...). The (provider, provider_user_id) pair is the provider's stable
-- subject and is UNIQUE per provider, so a returning user is always matched to
-- the same forge account on every login. No provider secrets, access tokens, or
-- refresh tokens are ever stored here — only the opaque subject id and the
-- email the provider reported at link time (kept for auditing / display).
--
-- Additive migration: no existing table is altered, so this is safe to apply on
-- top of any prior schema. The integrator finalizes the numeric ordering.

CREATE TABLE IF NOT EXISTS oauth_identities (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id           UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    provider          TEXT NOT NULL,
    provider_user_id  TEXT NOT NULL,
    email             TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- One provider account maps to exactly one forge user.
    UNIQUE (provider, provider_user_id)
);

-- Fast reverse lookup: "which external identities are linked to this user".
CREATE INDEX IF NOT EXISTS oauth_identities_user_id_idx ON oauth_identities (user_id);
