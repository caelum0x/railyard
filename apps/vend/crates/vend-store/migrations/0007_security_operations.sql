-- Production security operations. Audit rows are deliberately append-only;
-- neither application roles nor accidental ORM calls may rewrite history.

CREATE TABLE vend_audit_events (
    id             TEXT PRIMARY KEY,
    occurred_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    tenant_id      TEXT REFERENCES tenants(id),
    actor_kind     TEXT NOT NULL CHECK (actor_kind IN
                       ('administrator', 'customer', 'provisioning_key', 'system')),
    actor_id       TEXT,
    action         TEXT NOT NULL CHECK (length(action) BETWEEN 3 AND 128),
    target_kind    TEXT CHECK (target_kind IS NULL OR length(target_kind) BETWEEN 1 AND 64),
    target_id      TEXT CHECK (target_id IS NULL OR length(target_id) BETWEEN 1 AND 255),
    outcome        TEXT NOT NULL CHECK (outcome IN ('succeeded', 'denied', 'failed')),
    request_id     TEXT CHECK (request_id IS NULL OR length(request_id) BETWEEN 1 AND 128),
    source_hash    BYTEA CHECK (source_hash IS NULL OR octet_length(source_hash) = 32),
    metadata       JSONB NOT NULL DEFAULT '{}'::jsonb,
    CHECK (jsonb_typeof(metadata) = 'object')
);

CREATE INDEX vend_audit_events_tenant_time
    ON vend_audit_events (tenant_id, occurred_at DESC, id DESC);
CREATE INDEX vend_audit_events_action_time
    ON vend_audit_events (action, occurred_at DESC);

CREATE FUNCTION vend_reject_audit_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'vend_audit_events is append-only';
END;
$$;

CREATE TRIGGER vend_audit_events_no_update
    BEFORE UPDATE OR DELETE ON vend_audit_events
    FOR EACH ROW EXECUTE FUNCTION vend_reject_audit_mutation();

-- A fixed-window counter is sufficient here because it is enforced inside
-- Postgres and therefore shared by every Vend replica. Subjects are keyed by a
-- peppered digest; plaintext email addresses, bearer tokens, OAuth state and
-- provisioning keys never enter this table.
CREATE TABLE vend_rate_limits (
    bucket             TEXT NOT NULL CHECK (length(bucket) BETWEEN 1 AND 64),
    tenant_scope       TEXT NOT NULL CHECK (length(tenant_scope) BETWEEN 1 AND 128),
    subject_hash       BYTEA NOT NULL CHECK (octet_length(subject_hash) = 32),
    window_started_at  TIMESTAMPTZ NOT NULL,
    attempts           INTEGER NOT NULL CHECK (attempts > 0),
    blocked_until      TIMESTAMPTZ,
    updated_at         TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (bucket, tenant_scope, subject_hash)
);

CREATE INDEX vend_rate_limits_cleanup
    ON vend_rate_limits (updated_at);
