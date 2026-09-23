-- Phase E1: external CI runners + check runs.
--
-- This is the external-runner contract for rithub. A *runner* is an out-of-band
-- worker that registers once with a bearer token (only its SHA-256 hash is
-- stored, like PATs and deploy keys), listens for `check_run` webhook events
-- dispatched on push/PR, executes the job, and reports progress back through the
-- check-run POST/PATCH API.
--
-- `check_runs` is the richer GitHub-Checks-style status model (queued →
-- in_progress → completed + a typed conclusion). It coexists with the simpler
-- `commit_statuses` table (0006): statuses are the lightweight external-CI ping,
-- check_runs are first-class runner-owned jobs with output and timing.

-- --- external CI runners ----------------------------------------------------
CREATE TABLE IF NOT EXISTS ci_runners (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id      UUID        NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name         TEXT        NOT NULL,
    -- SHA-256 hash of the runner's bearer token. The plaintext is shown once at
    -- registration and never stored (identical posture to tokens/deploy_keys).
    token_hash   TEXT        NOT NULL UNIQUE,
    -- NULL when the registering user has been deleted.
    created_by   UUID        REFERENCES users(id) ON DELETE SET NULL,
    -- Updated every time the runner authenticates (liveness signal).
    last_seen_at TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- A runner name is unique within its repository.
    UNIQUE (repo_id, name)
);

CREATE INDEX IF NOT EXISTS ci_runners_repo_idx  ON ci_runners (repo_id);
-- Token auth is a hot path on every report.
CREATE INDEX IF NOT EXISTS ci_runners_token_idx ON ci_runners (token_hash);

-- --- check runs -------------------------------------------------------------
CREATE TABLE IF NOT EXISTS check_runs (
    id             UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id        UUID        NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    -- The runner that owns this check run (NULL for a placeholder queued by the
    -- dispatcher before any runner has claimed it).
    runner_id      UUID        REFERENCES ci_runners(id) ON DELETE SET NULL,
    head_sha       TEXT        NOT NULL,
    -- The check/context name, e.g. "build", "test", "lint".
    name           TEXT        NOT NULL,
    -- The runner's own opaque job id, optional (for correlation on its side).
    external_id    TEXT,
    status         TEXT        NOT NULL DEFAULT 'queued'
                       CHECK (status IN ('queued', 'in_progress', 'completed')),
    -- Only meaningful once status = 'completed'.
    conclusion     TEXT
                       CHECK (conclusion IN ('success', 'failure', 'neutral',
                                             'cancelled', 'timed_out',
                                             'action_required', 'skipped')),
    details_url    TEXT,
    output_title   TEXT,
    output_summary TEXT,
    started_at     TIMESTAMPTZ,
    completed_at   TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Surface check status on a commit/PR: newest run per (repo, sha, name).
CREATE INDEX IF NOT EXISTS check_runs_lookup_idx
    ON check_runs (repo_id, head_sha, name, created_at DESC);
