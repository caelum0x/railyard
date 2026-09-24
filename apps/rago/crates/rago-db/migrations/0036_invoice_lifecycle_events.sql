-- Invoice / subscription / order lifecycle: draft grace periods, order-form
-- expiry, VIES results, and the indexes the integration sweep relies on.

-- Organization-wide draft grace period in days (Lago
-- `organizations.invoice_grace_period`). While an invoice is inside its grace
-- window it stays `draft` (editable, refreshable, deletable, unnumbered); the
-- FinalizeInvoices clock finalizes it once `issuing_date + grace` has passed.
-- `customers.invoice_grace_period` (0021) overrides it per customer.
ALTER TABLE organizations ADD COLUMN IF NOT EXISTS invoice_grace_period integer NOT NULL DEFAULT 0;

-- `invoice.ready_to_finalize` is emitted once per draft, the day before its
-- grace period ends; this stamp makes that exactly-once across clock ticks.
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS ready_to_finalize_notified_at timestamp(6);

-- Drafts are selected by the finalize / refresh clocks on every tick.
CREATE INDEX IF NOT EXISTS idx_invoices_drafts
    ON invoices (organization_id, issuing_date) WHERE status = 0;

-- Order forms expire (Lago `order_forms.expires_at`, inherited from the
-- approved quote version's expiry). An unsigned form past its expiry is moved
-- to `expired` by the ExpireOrderForms clock, which emits `order_form.expired`.
ALTER TABLE order_forms ADD COLUMN IF NOT EXISTS expires_at timestamp(6);
ALTER TABLE order_forms ADD COLUMN IF NOT EXISTS expired_at timestamp(6);
CREATE INDEX IF NOT EXISTS idx_order_forms_expiring
    ON order_forms (expires_at) WHERE status = 'draft' AND expires_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_order_forms_scheduled
    ON order_forms (execute_at) WHERE status = 'signed' AND execute_at IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_quote_versions_expiring
    ON quote_versions (expires_at) WHERE status = 'approved' AND expires_at IS NOT NULL;

-- EU VIES validation of a customer's VAT number (`customer.vies_check`).
ALTER TABLE customers ADD COLUMN IF NOT EXISTS vies_valid boolean;
ALTER TABLE customers ADD COLUMN IF NOT EXISTS vies_checked_at timestamp(6);
ALTER TABLE customers ADD COLUMN IF NOT EXISTS vies_checked_number varchar;
ALTER TABLE customers ADD COLUMN IF NOT EXISTS vies_name varchar;
ALTER TABLE customers ADD COLUMN IF NOT EXISTS vies_address text;
ALTER TABLE customers ADD COLUMN IF NOT EXISTS vies_error varchar;

-- Integration sweep (`unsynced_resources` / `unreported_tax_credit_notes`):
-- the NOT EXISTS probes look jobs up by type + payload id, and mappings by
-- connection + resource; the candidate scan filters by org + status + recency.
CREATE INDEX IF NOT EXISTS idx_jobs_type_invoice_id
    ON jobs (job_type, (payload->>'invoice_id'));
CREATE INDEX IF NOT EXISTS idx_jobs_type_credit_note_id
    ON jobs (job_type, (payload->>'credit_note_id'));
CREATE INDEX IF NOT EXISTS idx_integration_mappings_connection
    ON integration_mappings (integration_connection_id, resource_type, lago_id)
    WHERE deleted_at IS NULL;
CREATE INDEX IF NOT EXISTS idx_invoices_org_status_updated
    ON invoices (organization_id, status, updated_at);
CREATE INDEX IF NOT EXISTS idx_credit_notes_org_status_updated
    ON credit_notes (organization_id, status, updated_at);
