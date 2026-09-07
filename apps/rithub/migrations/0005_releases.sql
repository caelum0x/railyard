-- Phase 4: releases. A release names a tag (which points at a commit) with
-- optional notes. The tarball is produced on demand from the git tree (rit),
-- so no binary asset is stored in Postgres.

CREATE TABLE IF NOT EXISTS releases (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id      UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    tag          TEXT NOT NULL,
    name         TEXT NOT NULL,
    body         TEXT,
    target_sha   TEXT NOT NULL,
    author_id    UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (repo_id, tag)
);

CREATE INDEX IF NOT EXISTS releases_repo_id_idx ON releases (repo_id);
