-- Phase 3 (bonus): outbound webhooks. A repository can subscribe a URL to a set
-- of events; deliveries are HMAC-SHA256 signed and recorded with their outcome.

CREATE TABLE IF NOT EXISTS webhooks (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id     UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    url         TEXT NOT NULL,
    secret      TEXT NOT NULL,
    events      TEXT[] NOT NULL DEFAULT '{}',   -- e.g. {'pr.opened','pr.merged','issue.opened'}
    active      BOOLEAN NOT NULL DEFAULT true,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS webhooks_repo_id_idx ON webhooks (repo_id);

CREATE TABLE IF NOT EXISTS webhook_deliveries (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    webhook_id   UUID NOT NULL REFERENCES webhooks(id) ON DELETE CASCADE,
    event        TEXT NOT NULL,
    success      BOOLEAN NOT NULL,
    status_code  INTEGER,
    attempts     INTEGER NOT NULL,
    error        TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS webhook_deliveries_webhook_id_idx ON webhook_deliveries (webhook_id);
