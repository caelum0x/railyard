-- Postpaid (meter-only) batch ownership. Prepaid batches are tracked by the
-- billing outbox; meter-only deployments have no hold, so the gateway records
-- which API key started each provider batch and which retail selector prices
-- it. The completed batch is metered exactly once, attributed to the owner
-- (not to whichever key happens to poll it).
CREATE TABLE IF NOT EXISTS relay_metered_batches (
    batch_id UUID PRIMARY KEY,
    owner_principal TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    metered_at TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS relay_metered_batches_owner_idx
    ON relay_metered_batches (owner_principal, created_at DESC);
