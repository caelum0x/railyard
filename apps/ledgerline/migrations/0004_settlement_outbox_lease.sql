-- One driver per settlement saga. A coordinator claims the row's lease before
-- it calls Mint, renews it before every Mint step, and every saga write is
-- fenced on the lease owner, so a second instance (or a recovery sweep) can
-- neither drive the same saga at once nor overwrite its progress with a stale
-- copy. An expired lease (a crashed driver) can be claimed by anyone.
ALTER TABLE settlement_outbox ADD COLUMN IF NOT EXISTS lease_owner TEXT;
ALTER TABLE settlement_outbox ADD COLUMN IF NOT EXISTS lease_until TIMESTAMPTZ;
