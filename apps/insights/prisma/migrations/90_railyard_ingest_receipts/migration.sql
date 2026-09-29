-- Railyard fork addition: de-duplicates retried edge-collector batches by
-- (org_id, sent_at, site_key) (ingest contract v1). Idempotent. Rows older than
-- a day can be deleted by the retention job; nothing reads them after the retry window.
CREATE TABLE IF NOT EXISTS "railyard_ingest_receipt" (
    "org_id" UUID NOT NULL,
    "sent_at" VARCHAR(40) NOT NULL,
    "site_key" VARCHAR(100) NOT NULL,
    "received_at" TIMESTAMPTZ(6) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT "railyard_ingest_receipt_pkey" PRIMARY KEY ("org_id", "sent_at", "site_key")
);
CREATE INDEX IF NOT EXISTS "railyard_ingest_receipt_received_at_idx" ON "railyard_ingest_receipt" ("received_at");
