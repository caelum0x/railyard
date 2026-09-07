-- rithub forge schema (F0 foundation): users, organizations, repositories,
-- and org memberships. Repositories reference a bare git repo on disk by
-- `disk_path`; the git object/ref plane is owned by `rit`, never by Postgres.
-- Append-only migration: never edit in place, add a new numbered file instead.

CREATE TABLE IF NOT EXISTS users (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    username    TEXT NOT NULL UNIQUE,
    email       TEXT NOT NULL UNIQUE,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS organizations (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name          TEXT NOT NULL UNIQUE,
    display_name  TEXT,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS repositories (
    id              UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    org_id          UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name            TEXT NOT NULL,
    description     TEXT,
    default_branch  TEXT NOT NULL DEFAULT 'main',
    disk_path       TEXT NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (org_id, name)
);

CREATE INDEX IF NOT EXISTS repositories_org_id_idx ON repositories (org_id);

-- Org membership with a coarse role. `forge-rbac` will grow this into full
-- team/repo access resolution; F0 only needs owner/member on an org.
CREATE TABLE IF NOT EXISTS memberships (
    id        UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id   UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    org_id    UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    role      TEXT NOT NULL DEFAULT 'member',
    UNIQUE (user_id, org_id)
);
