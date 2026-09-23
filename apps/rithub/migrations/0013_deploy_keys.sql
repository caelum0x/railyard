-- Phase A-2: Deploy keys.
-- A deploy key couples an SSH public key blob (for SSH transport auth) with a
-- hashed bearer token (for HTTP Basic auth) and is scoped to exactly one
-- repository. read_only = TRUE means the key may only clone/fetch.

CREATE TABLE IF NOT EXISTS deploy_keys (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id     UUID        NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    title       TEXT        NOT NULL DEFAULT 'deploy key',
    -- Base64 blob of the SSH public key (same format as ssh_keys.key_blob).
    key_blob    TEXT        NOT NULL,
    -- SHA-256 hash of the bearer token used for HTTP Basic auth.
    -- The plaintext is shown once at creation time and never stored.
    token_hash  TEXT        NOT NULL UNIQUE,
    read_only   BOOLEAN     NOT NULL DEFAULT TRUE,
    -- NULL when the creating user has been deleted.
    created_by  UUID        REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A given public key may only be registered once per repository.
    UNIQUE (repo_id, key_blob)
);

CREATE INDEX IF NOT EXISTS deploy_keys_repo_idx  ON deploy_keys (repo_id);
-- Auth lookups: both SSH (by blob) and HTTP Basic (by token_hash) are hot paths.
CREATE INDEX IF NOT EXISTS deploy_keys_blob_idx  ON deploy_keys (key_blob);
CREATE INDEX IF NOT EXISTS deploy_keys_token_idx ON deploy_keys (token_hash);
