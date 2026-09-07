-- Persist the exact challenge that owns each nonce. Replica B can therefore
-- verify what replica A actually issued instead of trusting its local config.
ALTER TABLE turnstile_nonces
    ADD COLUMN IF NOT EXISTS challenge_json TEXT;

-- Pre-upgrade rows contain NULL and fail closed until their short TTL expires.
