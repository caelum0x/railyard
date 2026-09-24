-- Charge filters carry the event-property values they match
-- (Lago `charge_filter_values`: `{ "<property key>": ["value", ...] }`), set
-- from the REST `charge.filters[].values` input and the console charge form.
ALTER TABLE charge_filters ADD COLUMN IF NOT EXISTS "values" jsonb NOT NULL DEFAULT '{}';
CREATE INDEX IF NOT EXISTS idx_charge_filters_charge_live
    ON charge_filters (charge_id) WHERE deleted_at IS NULL;
