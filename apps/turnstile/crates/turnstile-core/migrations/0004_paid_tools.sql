-- Runtime-editable paid tools. The operator console / admin API writes here;
-- every replica routes, prices and advertises from these rows and reloads when
-- SUM(version) (the store revision) changes. `TURNSTILE_TOOLS_PATH` is only a
-- one-time seed applied while this table is empty.
--
-- Deletion is soft (`deleted = TRUE`): the name stays reserved and historical
-- settlements for the tool's resource remain attributable.
CREATE TABLE IF NOT EXISTS turnstile_paid_tools (
    name        TEXT PRIMARY KEY,
    spec_json   JSONB NOT NULL,
    enabled     BOOLEAN NOT NULL DEFAULT TRUE,
    deleted     BOOLEAN NOT NULL DEFAULT FALSE,
    version     BIGINT NOT NULL DEFAULT 1 CHECK (version > 0),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Append-only audit trail of every admin edit: who, when, old and new values.
CREATE TABLE IF NOT EXISTS turnstile_tool_audit (
    id              BIGSERIAL PRIMARY KEY,
    tool_name       TEXT NOT NULL,
    action          TEXT NOT NULL CHECK (action IN (
        'seed', 'create', 'update', 'enable', 'disable', 'delete'
    )),
    actor           TEXT NOT NULL,
    old_spec_json   JSONB,
    new_spec_json   JSONB,
    old_enabled     BOOLEAN,
    new_enabled     BOOLEAN,
    at              TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS turnstile_tool_audit_tool_idx
    ON turnstile_tool_audit (tool_name, id DESC);
