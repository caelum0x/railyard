-- Fixed charges get Lago's `code` (unique per plan among live rows), so the
-- REST API can address them as /plans/:code/fixed_charges/:fixed_charge_code.
ALTER TABLE fixed_charges ADD COLUMN IF NOT EXISTS code varchar;
UPDATE fixed_charges SET code = id::text WHERE code IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS index_fixed_charges_on_plan_id_and_code
    ON fixed_charges (plan_id, code) WHERE deleted_at IS NULL;
