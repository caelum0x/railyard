-- Provider API keys managed from the Relay UI. Values are sealed with
-- AES-256-GCM under the gateway's RELAY_CREDENTIALS_KEY before they reach
-- Postgres (the credential name is bound as associated data), and are only
-- ever decrypted inside the gateway to be passed as TensorZero `dynamic::`
-- credentials. The API never returns a stored secret, only `key_hint`.
CREATE TABLE IF NOT EXISTS relay_provider_credentials (
    credential_name TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    nonce BYTEA NOT NULL,
    ciphertext BYTEA NOT NULL,
    key_hint TEXT NOT NULL,
    updated_by TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_tested_at TIMESTAMPTZ,
    last_test_ok BOOLEAN,
    last_test_message TEXT
);
