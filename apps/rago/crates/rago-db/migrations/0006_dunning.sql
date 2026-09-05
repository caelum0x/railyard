-- Dunning: mark finalized, unpaid invoices as overdue.
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS payment_overdue boolean DEFAULT false NOT NULL;
ALTER TABLE invoices ADD COLUMN IF NOT EXISTS payment_due_date date;
