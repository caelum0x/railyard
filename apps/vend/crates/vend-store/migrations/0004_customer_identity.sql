-- Vend-owned customer identity. Principals are global because one human may be
-- a member of several tenants; authorization and status are per membership.
-- Passwords use Argon2id. Setup/session/state bearers have 256 bits of CSPRNG
-- entropy and only SHA-256 digests are stored.

CREATE TABLE customer_users (
    id               TEXT PRIMARY KEY,
    email            TEXT NOT NULL,
    email_normalized TEXT NOT NULL UNIQUE,
    display_name     TEXT NOT NULL,
    password_hash    TEXT,
    status           TEXT NOT NULL CHECK (status IN ('pending', 'active', 'disabled')),
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    activated_at     TIMESTAMPTZ
);

CREATE TABLE tenant_memberships (
    id          TEXT PRIMARY KEY,
    user_id     TEXT NOT NULL REFERENCES customer_users(id) ON DELETE CASCADE,
    tenant_id   TEXT NOT NULL REFERENCES tenants(id),
    role        TEXT NOT NULL CHECK (role IN ('owner', 'admin', 'viewer')),
    status      TEXT NOT NULL CHECK (status IN ('invited', 'active', 'disabled')),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    activated_at TIMESTAMPTZ,
    UNIQUE (user_id, tenant_id)
);

CREATE INDEX tenant_memberships_tenant_created
    ON tenant_memberships (tenant_id, created_at, id);

CREATE TABLE customer_external_identities (
    provider    TEXT NOT NULL CHECK (provider IN ('github', 'google')),
    subject     TEXT NOT NULL,
    user_id     TEXT NOT NULL REFERENCES customer_users(id) ON DELETE CASCADE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, subject),
    UNIQUE (provider, user_id)
);

CREATE TABLE tenant_setup_tokens (
    id            TEXT PRIMARY KEY,
    membership_id TEXT NOT NULL REFERENCES tenant_memberships(id) ON DELETE CASCADE,
    prefix        TEXT NOT NULL UNIQUE,
    secret_hash   BYTEA NOT NULL CHECK (octet_length(secret_hash) = 32),
    expires_at    TIMESTAMPTZ NOT NULL,
    consumed_at   TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX tenant_setup_tokens_membership_active
    ON tenant_setup_tokens (membership_id, expires_at)
    WHERE consumed_at IS NULL;

CREATE TABLE tenant_sessions (
    id            TEXT PRIMARY KEY,
    membership_id TEXT NOT NULL REFERENCES tenant_memberships(id) ON DELETE CASCADE,
    prefix        TEXT NOT NULL UNIQUE,
    secret_hash   BYTEA NOT NULL CHECK (octet_length(secret_hash) = 32),
    expires_at    TIMESTAMPTZ NOT NULL,
    revoked_at    TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX tenant_sessions_membership_active
    ON tenant_sessions (membership_id, expires_at)
    WHERE revoked_at IS NULL;

-- PKCE verifier and OIDC nonce are short-lived server-side flow state. They are
-- deleted from usefulness by the same atomic update that consumes `state`.
CREATE TABLE oauth_login_states (
    id            TEXT PRIMARY KEY,
    tenant_id     TEXT NOT NULL REFERENCES tenants(id),
    provider      TEXT NOT NULL CHECK (provider IN ('github', 'google')),
    prefix        TEXT NOT NULL UNIQUE,
    secret_hash   BYTEA NOT NULL CHECK (octet_length(secret_hash) = 32),
    code_verifier TEXT NOT NULL,
    nonce         TEXT NOT NULL,
    expires_at    TIMESTAMPTZ NOT NULL,
    consumed_at   TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX oauth_login_states_active
    ON oauth_login_states (expires_at)
    WHERE consumed_at IS NULL;
