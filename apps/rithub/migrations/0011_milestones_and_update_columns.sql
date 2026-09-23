-- Phase C: milestones + FK wiring for update endpoints.
--
-- Milestones group issues and pull requests under a shared goal with an optional
-- due date. Numbers are per-repo sequential (same `next_repo_number` lock as
-- issues/PRs). Progress (open/closed counts) is computed on read.
--
-- The ALTER TABLE statements add `milestone_id` to the two existing tables;
-- this is the only DDL needed to back the new PATCH/PUT update endpoints — all
-- other update logic (rename, description, visibility, default_branch) acts on
-- columns that already exist.

CREATE TABLE IF NOT EXISTS milestones (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id      UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    number       INTEGER NOT NULL,
    title        TEXT NOT NULL,
    description  TEXT,
    due_on       TIMESTAMPTZ,
    closed_at    TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, number)
);

CREATE INDEX IF NOT EXISTS milestones_repo_number_idx ON milestones (repo_id, number);

-- Attach milestone FK to issues and pull requests (nullable, SET NULL on delete
-- so removing a milestone does not silently destroy the linked items).
ALTER TABLE issues
    ADD COLUMN IF NOT EXISTS milestone_id UUID REFERENCES milestones(id) ON DELETE SET NULL;

ALTER TABLE pull_requests
    ADD COLUMN IF NOT EXISTS milestone_id UUID REFERENCES milestones(id) ON DELETE SET NULL;
