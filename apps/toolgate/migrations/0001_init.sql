-- Switchyard (apps/toolgate) initial schema. IDs are prefixed ULIDs, money is
-- BIGINT micro-USD, timestamps are timestamptz.

CREATE TABLE IF NOT EXISTS orgs (
    id                    TEXT PRIMARY KEY,
    slug                  TEXT NOT NULL UNIQUE,
    name                  TEXT NOT NULL,
    mint_account_id       TEXT,
    rago_subscription_id  TEXT,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS projects (
    id                  TEXT PRIMARY KEY,
    org_id              TEXT NOT NULL REFERENCES orgs(id),
    name                TEXT NOT NULL,
    monthly_cap_micros  BIGINT,
    daily_cap_micros    BIGINT,
    tool_daily_caps     JSONB NOT NULL DEFAULT '{}'::jsonb,
    routing_policy      TEXT NOT NULL DEFAULT 'free_first'
                        CHECK (routing_policy IN ('free_first','cheapest','fastest','pinned')),
    egress_extra_hosts  JSONB NOT NULL DEFAULT '[]'::jsonb,
    pinned_tools        TEXT[] NOT NULL DEFAULT '{}',
    daily_send_limit    BIGINT,
    created_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS projects_org ON projects(org_id);

CREATE TABLE IF NOT EXISTS members (
    org_id   TEXT NOT NULL REFERENCES orgs(id),
    user_id  TEXT NOT NULL,
    role     TEXT NOT NULL CHECK (role IN ('owner','admin','member','viewer')),
    PRIMARY KEY (org_id, user_id)
);

-- Binds a Relay `sk-t0-<public_id>-...` key to a project and scopes. The key
-- itself lives in Relay's key store; there is no second key format.
CREATE TABLE IF NOT EXISTS api_keys (
    public_id                 TEXT PRIMARY KEY,
    project_id                TEXT NOT NULL REFERENCES projects(id),
    description               TEXT NOT NULL DEFAULT '',
    scopes                    TEXT[] NOT NULL,
    tool_allow                TEXT[],
    per_key_daily_cap_micros  BIGINT,
    extra_projects            TEXT[] NOT NULL DEFAULT '{}',
    revoked_at                TIMESTAMPTZ,
    created_at                TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS api_keys_project ON api_keys(project_id);

CREATE TABLE IF NOT EXISTS oauth_clients (
    client_id      TEXT PRIMARY KEY,
    kind           TEXT NOT NULL CHECK (kind IN ('dcr','cimd','first_party')),
    metadata       JSONB NOT NULL,
    redirect_uris  TEXT[] NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS oauth_codes (
    code_hash       TEXT PRIMARY KEY,
    client_id       TEXT NOT NULL,
    user_id         TEXT NOT NULL,
    org_id          TEXT NOT NULL,
    project_id      TEXT NOT NULL,
    scopes          TEXT[] NOT NULL,
    redirect_uri    TEXT NOT NULL,
    code_challenge  TEXT NOT NULL,
    resource        TEXT NOT NULL,
    expires_at      TIMESTAMPTZ NOT NULL
);

-- Refresh tokens are stored hashed and rotate; reuse of a consumed token
-- revokes the whole family.
CREATE TABLE IF NOT EXISTS oauth_grants (
    id            TEXT PRIMARY KEY,
    family_id     TEXT NOT NULL,
    client_id     TEXT NOT NULL,
    user_id       TEXT NOT NULL,
    org_id        TEXT NOT NULL,
    project_id    TEXT NOT NULL,
    scopes        TEXT[] NOT NULL,
    refresh_hash  TEXT NOT NULL UNIQUE,
    resource      TEXT NOT NULL,
    expires_at    TIMESTAMPTZ NOT NULL,
    used_at       TIMESTAMPTZ,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS oauth_grants_family ON oauth_grants(family_id);
CREATE TABLE IF NOT EXISTS oauth_revoked_families (
    family_id   TEXT PRIMARY KEY,
    revoked_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE TABLE IF NOT EXISTS oauth_consents (
    family_id   TEXT NOT NULL,
    tool_id     TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (family_id, tool_id)
);
CREATE TABLE IF NOT EXISTS device_codes (
    device_hash   TEXT PRIMARY KEY,
    user_code     TEXT NOT NULL UNIQUE,
    client_id     TEXT NOT NULL,
    scopes        TEXT[] NOT NULL,
    status        TEXT NOT NULL CHECK (status IN ('pending','approved','denied','used')),
    user_id       TEXT,
    org_id        TEXT,
    project_id    TEXT,
    expires_at    TIMESTAMPTZ NOT NULL,
    last_poll_at  TIMESTAMPTZ,
    interval_s    INT NOT NULL DEFAULT 5
);

-- Catalog mirror (the YAML manifests are the source; synced at startup).
CREATE TABLE IF NOT EXISTS providers (
    id             TEXT PRIMARY KEY,
    kind           TEXT NOT NULL CHECK (kind IN ('own','public','byo','platform')),
    egress_hosts   TEXT[] NOT NULL,
    tos_resale_ok  BOOLEAN NOT NULL DEFAULT false,
    manifest       JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS capabilities (
    id                       TEXT PRIMARY KEY,
    title                    TEXT NOT NULL,
    description              TEXT NOT NULL,
    canonical_input_schema   JSONB NOT NULL,
    canonical_output_schema  JSONB NOT NULL
);
CREATE TABLE IF NOT EXISTS tools (
    id             TEXT PRIMARY KEY,
    capability_id  TEXT NOT NULL,
    provider_id    TEXT NOT NULL,
    version        INT NOT NULL,
    input_schema   JSONB,
    output_schema  JSONB,
    side_effect    TEXT NOT NULL CHECK (side_effect IN ('read','write','send','spend')),
    price          JSONB NOT NULL,
    cache_ttl_s    BIGINT NOT NULL,
    idempotent     BOOLEAN NOT NULL,
    status         TEXT NOT NULL,
    manifest       JSONB NOT NULL
);

-- Envelope encryption: one wrapped data key per org.
CREATE TABLE IF NOT EXISTS org_keys (
    org_id       TEXT PRIMARY KEY REFERENCES orgs(id),
    dek_wrapped  BYTEA NOT NULL,
    kek_version  INT NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS credentials (
    id           TEXT PRIMARY KEY,
    org_id       TEXT NOT NULL REFERENCES orgs(id),
    provider_id  TEXT NOT NULL,
    name         TEXT NOT NULL,
    kind         TEXT NOT NULL CHECK (kind IN ('api_key','oauth','basic','header_set')),
    ciphertext   BYTEA NOT NULL,
    nonce        BYTEA NOT NULL,
    meta         JSONB NOT NULL DEFAULT '{}'::jsonb,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_ok_at   TIMESTAMPTZ,
    last_error   TEXT
);
CREATE INDEX IF NOT EXISTS credentials_org_provider ON credentials(org_id, provider_id, created_at DESC);

CREATE TABLE IF NOT EXISTS bundles (
    id               TEXT NOT NULL,
    org_id           TEXT NOT NULL,
    name             TEXT NOT NULL,
    description      TEXT NOT NULL DEFAULT '',
    skill_md         TEXT NOT NULL,
    tool_ids         TEXT[] NOT NULL,
    credential_refs  TEXT[] NOT NULL DEFAULT '{}',
    version          INT NOT NULL DEFAULT 1,
    updated_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, id)
);

CREATE TABLE IF NOT EXISTS byo_tools (
    id             TEXT NOT NULL,
    org_id         TEXT NOT NULL REFERENCES orgs(id),
    base_url       TEXT NOT NULL CHECK (base_url LIKE 'https://%'),
    credential_id  TEXT,
    header         TEXT NOT NULL,
    prefix         TEXT NOT NULL DEFAULT '',
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, id)
);

-- Append-only, hash-chained audit of every call (per project).
CREATE TABLE IF NOT EXISTS calls (
    id               TEXT PRIMARY KEY,
    project_id       TEXT NOT NULL,
    org_id           TEXT NOT NULL,
    seq              BIGINT NOT NULL,
    key_public_id    TEXT NOT NULL,
    target           TEXT NOT NULL,
    tool_id          TEXT,
    provider_id      TEXT,
    side_effect      TEXT NOT NULL,
    idempotency_key  TEXT,
    request_hash     TEXT NOT NULL,
    status           TEXT NOT NULL,
    http_status      INT NOT NULL,
    latency_ms       BIGINT NOT NULL,
    cost_micros      BIGINT NOT NULL,
    price_basis      TEXT NOT NULL,
    mint_hold_id     TEXT,
    mint_receipt     TEXT,
    rago_event_id    TEXT,
    cache            TEXT NOT NULL,
    fallback_chain   JSONB NOT NULL DEFAULT '[]'::jsonb,
    created_at       TIMESTAMPTZ NOT NULL,
    prev_hash        TEXT NOT NULL,
    row_hash         TEXT NOT NULL,
    UNIQUE (project_id, seq)
);
CREATE INDEX IF NOT EXISTS calls_project_created ON calls(project_id, created_at);

-- The audit log is append-only: refuse UPDATE and DELETE.
CREATE OR REPLACE FUNCTION switchyard_calls_append_only() RETURNS trigger AS $$
BEGIN
    RAISE EXCEPTION 'calls is append-only';
END;
$$ LANGUAGE plpgsql;
DROP TRIGGER IF EXISTS calls_append_only ON calls;
CREATE TRIGGER calls_append_only BEFORE UPDATE OR DELETE ON calls
    FOR EACH ROW EXECUTE FUNCTION switchyard_calls_append_only();

CREATE TABLE IF NOT EXISTS idempotency (
    project_id    TEXT NOT NULL,
    key           TEXT NOT NULL,
    request_hash  TEXT NOT NULL,
    http_status   INT NOT NULL,
    response      JSONB NOT NULL,
    expires_at    TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (project_id, key)
);

CREATE TABLE IF NOT EXISTS outbox (
    id               TEXT PRIMARY KEY,
    kind             TEXT NOT NULL,
    payload          JSONB NOT NULL,
    attempts         INT NOT NULL DEFAULT 0,
    next_attempt_at  TIMESTAMPTZ NOT NULL,
    delivered_at     TIMESTAMPTZ,
    last_error       TEXT
);
CREATE INDEX IF NOT EXISTS outbox_due ON outbox(next_attempt_at) WHERE delivered_at IS NULL;

CREATE TABLE IF NOT EXISTS provider_stats (
    tool_id               TEXT PRIMARY KEY,
    calls                 BIGINT NOT NULL DEFAULT 0,
    successes             BIGINT NOT NULL DEFAULT 0,
    total_latency_ms      BIGINT NOT NULL DEFAULT 0,
    observed_cost_micros  BIGINT NOT NULL DEFAULT 0
);
