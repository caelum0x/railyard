-- GraphQL admin CRUD support: the charge_filters catalog table plus two columns
-- the dashboard admin mutations persist (customer invoice grace period, invoice
-- payment-dispute-lost timestamp). Column names/types match Lago's
-- db/structure.sql so the port stays wire-compatible with a real Lago DB.

CREATE TABLE IF NOT EXISTS charge_filters (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    organization_id uuid NOT NULL,
    charge_id uuid NOT NULL,
    invoice_display_name varchar,
    properties jsonb DEFAULT '{}' NOT NULL,
    created_at timestamp(6) NOT NULL,
    updated_at timestamp(6) NOT NULL,
    deleted_at timestamp(6)
);
CREATE INDEX IF NOT EXISTS index_charge_filters_on_charge_id ON charge_filters (charge_id);

-- Customers::UpdateInvoiceGracePeriod persists this per-customer override.
ALTER TABLE customers ADD COLUMN IF NOT EXISTS invoice_grace_period integer;

-- Invoices::LoseDisputeService stamps this when a payment dispute is lost.
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS payment_dispute_lost_at timestamp(6);
