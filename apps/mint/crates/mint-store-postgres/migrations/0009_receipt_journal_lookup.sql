-- Signed transfer responses resolve the sender receipt by the operation's
-- journal sequence. Keep that lookup bounded as the append-only receipt table
-- grows; account_id remains in the key to enforce the intended chain owner.
--
-- Safety on a pre-existing (non-greenfield) database:
--   * Idempotent: if the index already exists this is a no-op, so re-runs and
--     partially-migrated databases converge cleanly (greenfield still gets the
--     unique index in one pass).
--   * `receipts` is append-only financial audit data, so this migration NEVER
--     deletes or rewrites rows to force uniqueness. If a legacy/dirty database
--     already holds duplicate (account_id, journal_seq) rows it STOPS with an
--     actionable error instead of the opaque Postgres "could not create unique
--     index ... duplicate key" failure — duplicate receipts signal a chain
--     integrity problem that a human must investigate, not something a
--     migration may silently resolve.
--
--   MANUAL DEDUP (only if this migration raises on `receipts_account_journal`):
--   inspect the offending groups, then resolve each under audit before re-running:
--
--       SELECT account_id, journal_seq, count(*) AS n,
--              array_agg(seq ORDER BY seq) AS receipt_seqs
--       FROM receipts
--       GROUP BY account_id, journal_seq
--       HAVING count(*) > 1;
--
--   For each group: verify the hash-linked chain, keep the canonical receipt,
--   and archive/repair the extras under audit; then re-run migrations. Note the
--   0008 append-only trigger (`mint_reject_mutation`) forbids UPDATE/DELETE on
--   `receipts`, so any dedup must be a deliberate, audited maintenance action
--   that temporarily lifts that guard — never an automatic step in this file.
DO $$
BEGIN
    -- Idempotent: index already present (re-run / partially migrated DB).
    IF to_regclass('public.receipts_account_journal') IS NOT NULL THEN
        RETURN;
    END IF;

    -- Guard: refuse to hard-fail opaquely on a dirty DB; surface an actionable
    -- error instead. Never mutate append-only receipts to force uniqueness.
    IF EXISTS (
        SELECT 1
        FROM receipts
        GROUP BY account_id, journal_seq
        HAVING count(*) > 1
    ) THEN
        RAISE EXCEPTION 'cannot create unique index receipts_account_journal: '
            'duplicate (account_id, journal_seq) rows exist; this indicates a '
            'receipt-chain integrity problem — see the manual dedup step in the '
            '0009 migration comment before re-running'
            USING ERRCODE = 'unique_violation';
    END IF;

    CREATE UNIQUE INDEX receipts_account_journal
        ON receipts (account_id, journal_seq);
END
$$;
