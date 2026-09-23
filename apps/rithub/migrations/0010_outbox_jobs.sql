-- Phase B: Durable job outbox for async side effects (webhooks, notifications,
-- audit appends, email). Workers claim jobs with SELECT … FOR UPDATE SKIP LOCKED
-- so multiple concurrent workers are safe without double-executing a job.

CREATE TABLE IF NOT EXISTS outbox_jobs (
    id          UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    kind        TEXT        NOT NULL,
    -- Structured payload; schema is per-kind (see rithub-core/src/jobs.rs).
    payload     JSONB       NOT NULL DEFAULT '{}',
    -- pending → running → succeeded|failed → (retry) → dead
    status      TEXT        NOT NULL DEFAULT 'pending'
                    CHECK (status IN ('pending', 'running', 'succeeded', 'failed', 'dead')),
    -- How many execution attempts have been made (0 = not yet tried).
    attempts    INT         NOT NULL DEFAULT 0,
    -- Worker must not pick up the job before this timestamp (used for backoff).
    run_after   TIMESTAMPTZ NOT NULL DEFAULT now(),
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Last error string, overwritten on each failure.
    last_error  TEXT
);

-- Partial index on the columns the worker scans: only pending/failed rows that
-- are due. The index is small and keeps claim queries fast even with a large
-- history of succeeded jobs.
CREATE INDEX IF NOT EXISTS outbox_jobs_ready_idx
    ON outbox_jobs (run_after, id)
    WHERE status IN ('pending', 'failed');
