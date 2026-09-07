-- Phase 6: SSH public keys. A user registers one or more public keys; the SSH
-- transport identifies the user by the key that signs the connection. We store
-- the base64 key blob (the middle field of an `authorized_keys` line) which is
-- what the SSH server compares against.

CREATE TABLE IF NOT EXISTS ssh_keys (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id      UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    title        TEXT NOT NULL DEFAULT 'key',
    key_blob     TEXT NOT NULL UNIQUE,   -- base64 of the public key (algo-independent)
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS ssh_keys_user_idx ON ssh_keys (user_id);
