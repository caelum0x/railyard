-- Integration OAuth connect + per-organization SSO.
--
-- `sso_states` becomes the single-use state store for every server-driven
-- OAuth redirect: SSO logins (`purpose = 'sso_login'`) and integration
-- connections (`purpose = 'integration_oauth'`). The state is bound to the
-- organization (and integration) it was minted for, so a callback can never be
-- replayed into another tenant.
ALTER TABLE sso_states ADD COLUMN IF NOT EXISTS purpose varchar NOT NULL DEFAULT 'sso_login';
ALTER TABLE sso_states ADD COLUMN IF NOT EXISTS organization_id uuid;
ALTER TABLE sso_states ADD COLUMN IF NOT EXISTS integration_id uuid;

-- The fetched provider catalog (items, accounts, subsidiaries, tax items) is
-- upserted per integration + type + external id.
CREATE UNIQUE INDEX IF NOT EXISTS idx_integration_items_unique
    ON integration_items (integration_id, item_type, external_id);

-- Auth (SSO) integrations are looked up by the email domain they serve.
CREATE INDEX IF NOT EXISTS idx_integration_connections_auth_domain
    ON integration_connections (lower(settings->>'domain'))
    WHERE integration_type = 'auth' AND deleted_at IS NULL;

-- Resource-mapping lookups by integration (sync status on invoices, credit
-- notes and customers).
CREATE INDEX IF NOT EXISTS idx_integration_customers_integration
    ON integration_customers (integration_id, customer_id);
