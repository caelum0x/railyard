-- Payment and CRM sync sweeps (`unsynced_payments`, `stale_crm_resources`).
--
-- Their jobs carry the synced record's id as `payload->>'resource_id'` and the
-- integration as `payload->>'integration_id'`; the NOT EXISTS probes look them
-- up by type + resource id (+ integration), so the probe stays an index lookup
-- as the jobs table grows.
CREATE INDEX IF NOT EXISTS idx_jobs_type_resource_id
    ON jobs (job_type, (payload->>'resource_id'), (payload->>'integration_id'));

-- Candidate scans: succeeded payments / changed customers and subscriptions of
-- one organization since the integration was connected.
CREATE INDEX IF NOT EXISTS idx_payments_org_status_updated
    ON payments (organization_id, status, updated_at);
CREATE INDEX IF NOT EXISTS idx_customers_org_updated
    ON customers (organization_id, updated_at) WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_subscriptions_org_updated
    ON subscriptions (organization_id, updated_at);
