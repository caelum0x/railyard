-- Webhook deliveries feed.
--
-- The existing `webhooks` table is the outbound-webhook *work item* (one row per
-- endpoint per lifecycle trigger, mutated in place as it is retried). It answers
-- "what is queued / retrying right now" but is overwritten on each attempt, so
-- it cannot answer "show me the delivery history of this endpoint".
--
-- `webhook_deliveries` is the append-only delivery log an operator reads: one
-- row recorded at each dispatch/attempt in the webhook path (jobs `record_webhook`
-- pending materialization, `emit_webhook` immediate delivery, and the incoming
-- provider-webhook re-emit), carrying the attempt count, the terminal HTTP
-- status, and the last error. It is a read surface — never mutated after insert.
CREATE TABLE IF NOT EXISTS webhook_deliveries (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    -- The configured endpoint this delivery targeted. `endpoint_id` is nullable:
    -- the incoming-provider re-emit path resolves only the URL (falling back to a
    -- default when no endpoint is configured), so the id may be unknown.
    endpoint_id uuid,
    endpoint varchar NOT NULL,
    event_type varchar NOT NULL,
    object_type varchar,
    payload jsonb DEFAULT '{}' NOT NULL,
    status varchar DEFAULT 'pending' NOT NULL,
    http_status integer,
    attempts integer DEFAULT 0 NOT NULL,
    last_error text,
    created_at timestamp(6) NOT NULL DEFAULT now(),
    delivered_at timestamp(6)
);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_org_created
    ON webhook_deliveries (organization_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_endpoint
    ON webhook_deliveries (endpoint_id);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_event_type
    ON webhook_deliveries (event_type);
CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_status
    ON webhook_deliveries (status);
