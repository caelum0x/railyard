-- Phase 3: the collaboration core — issues, pull requests, reviews, labels,
-- and a basic protected-branch rule. Numbers are per-repository and allocated
-- under a row lock on the owning repository (see the store), so issue/PR numbers
-- are stable and gap-free per repo.

-- --- labels -----------------------------------------------------------------
CREATE TABLE IF NOT EXISTS labels (
    id       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id  UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    name     TEXT NOT NULL,
    color    TEXT NOT NULL DEFAULT 'cccccc',
    UNIQUE (repo_id, name)
);

-- --- issues -----------------------------------------------------------------
CREATE TABLE IF NOT EXISTS issues (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id     UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    number      INTEGER NOT NULL,
    title       TEXT NOT NULL,
    body        TEXT,
    state       TEXT NOT NULL DEFAULT 'open',      -- 'open' | 'closed'
    author_id   UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    closed_at   TIMESTAMPTZ,
    UNIQUE (repo_id, number)
);

CREATE TABLE IF NOT EXISTS issue_comments (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    issue_id    UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    author_id   UUID REFERENCES users(id) ON DELETE SET NULL,
    body        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS issue_labels (
    issue_id  UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    label_id  UUID NOT NULL REFERENCES labels(id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, label_id)
);

CREATE TABLE IF NOT EXISTS issue_assignees (
    issue_id  UUID NOT NULL REFERENCES issues(id) ON DELETE CASCADE,
    user_id   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, user_id)
);

-- --- pull requests ----------------------------------------------------------
CREATE TABLE IF NOT EXISTS pull_requests (
    id                UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id           UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    number            INTEGER NOT NULL,
    title             TEXT NOT NULL,
    body              TEXT,
    state             TEXT NOT NULL DEFAULT 'open',   -- 'open' | 'closed' | 'merged'
    author_id         UUID REFERENCES users(id) ON DELETE SET NULL,
    base_branch       TEXT NOT NULL,
    head_branch       TEXT NOT NULL,
    base_sha          TEXT,                           -- base tip snapshot at open
    head_sha          TEXT,                           -- head tip snapshot at open
    merge_commit_sha  TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    merged_at         TIMESTAMPTZ,
    merged_by         UUID REFERENCES users(id) ON DELETE SET NULL,
    UNIQUE (repo_id, number)
);

CREATE TABLE IF NOT EXISTS pr_comments (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pr_id       UUID NOT NULL REFERENCES pull_requests(id) ON DELETE CASCADE,
    author_id   UUID REFERENCES users(id) ON DELETE SET NULL,
    body        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- --- reviews ----------------------------------------------------------------
CREATE TABLE IF NOT EXISTS reviews (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    pr_id        UUID NOT NULL REFERENCES pull_requests(id) ON DELETE CASCADE,
    reviewer_id  UUID REFERENCES users(id) ON DELETE SET NULL,
    state        TEXT NOT NULL,   -- 'approved' | 'changes_requested' | 'commented'
    body         TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Inline review thread comments (a path + optional line + body).
CREATE TABLE IF NOT EXISTS review_comments (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    review_id   UUID NOT NULL REFERENCES reviews(id) ON DELETE CASCADE,
    pr_id       UUID NOT NULL REFERENCES pull_requests(id) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    line        INTEGER,
    body        TEXT NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- --- basic protected-branch rule -------------------------------------------
CREATE TABLE IF NOT EXISTS branch_protections (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id             UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    branch              TEXT NOT NULL,
    required_approvals  INTEGER NOT NULL DEFAULT 1,
    UNIQUE (repo_id, branch)
);
