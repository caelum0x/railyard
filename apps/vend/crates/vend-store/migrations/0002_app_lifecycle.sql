-- Product lifecycle gate. Disabled apps remain inspectable in the control plane
-- while the public data plane stops resolving them for new inference requests.
ALTER TABLE apps
    ADD COLUMN IF NOT EXISTS enabled BOOLEAN NOT NULL DEFAULT true;

CREATE INDEX IF NOT EXISTS apps_tenant_created
    ON apps (tenant_id, created_at, id);

CREATE INDEX IF NOT EXISTS sub_keys_app_created
    ON sub_keys (app_id, created_at, id);

-- A key's app binding must belong to the same tenant as the key. The composite
-- uniqueness is only a referential target; app ids remain globally unique.
ALTER TABLE apps
    ADD CONSTRAINT apps_id_tenant_unique UNIQUE (id, tenant_id);

ALTER TABLE sub_keys
    ADD CONSTRAINT sub_keys_app_tenant_fk
    FOREIGN KEY (app_id, tenant_id) REFERENCES apps (id, tenant_id);
