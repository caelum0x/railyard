-- Phase 5: security & governance — fine-grained RBAC (per-repo collaborators +
-- teams), protected-branch push blocking + required status checks, CI commit
-- statuses, and an append-only audit log.

-- --- fine-grained per-repo access ------------------------------------------
CREATE TABLE IF NOT EXISTS repo_collaborators (
    repo_id  UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    user_id  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role     TEXT NOT NULL,               -- 'read' | 'write' | 'admin'
    PRIMARY KEY (repo_id, user_id)
);

CREATE TABLE IF NOT EXISTS teams (
    id       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id   UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name     TEXT NOT NULL,
    UNIQUE (org_id, name)
);

CREATE TABLE IF NOT EXISTS team_members (
    team_id  UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    user_id  UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    PRIMARY KEY (team_id, user_id)
);

CREATE TABLE IF NOT EXISTS team_repo_access (
    team_id  UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    repo_id  UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    role     TEXT NOT NULL,               -- 'read' | 'write' | 'admin'
    PRIMARY KEY (team_id, repo_id)
);

-- --- protected-branch rule: block direct push + required status checks ------
ALTER TABLE branch_protections
    ADD COLUMN IF NOT EXISTS block_direct_push BOOLEAN NOT NULL DEFAULT true;
ALTER TABLE branch_protections
    ADD COLUMN IF NOT EXISTS required_checks TEXT[] NOT NULL DEFAULT '{}';

-- --- CI commit statuses -----------------------------------------------------
CREATE TABLE IF NOT EXISTS commit_statuses (
    id           UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id      UUID NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    sha          TEXT NOT NULL,
    context      TEXT NOT NULL,
    state        TEXT NOT NULL,           -- 'pending' | 'success' | 'failure' | 'error'
    target_url   TEXT,
    description  TEXT,
    creator_id   UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS commit_statuses_lookup_idx
    ON commit_statuses (repo_id, sha, context, created_at DESC);

-- --- append-only audit log --------------------------------------------------
CREATE TABLE IF NOT EXISTS audit_events (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    repo_id     UUID REFERENCES repositories(id) ON DELETE CASCADE,
    actor_id    UUID REFERENCES users(id) ON DELETE SET NULL,
    actor_name  TEXT NOT NULL DEFAULT 'anonymous',
    action      TEXT NOT NULL,            -- e.g. 'push', 'push.rejected', 'pr.merged', ...
    target      TEXT,                     -- e.g. 'refs/heads/main', '#3', 'user:bob'
    detail      TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS audit_events_repo_idx ON audit_events (repo_id, created_at DESC);
