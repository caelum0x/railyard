-- Archive support: an archived passport stays readable (versions, provenance,
-- manifests) but refuses edits, locks, new versions and generation until it is
-- unarchived. NULL means active.
ALTER TABLE passports ADD COLUMN IF NOT EXISTS archived_at TIMESTAMPTZ;

CREATE INDEX IF NOT EXISTS passports_owner_active_created
    ON passports (owner, created_at DESC) WHERE archived_at IS NULL;
