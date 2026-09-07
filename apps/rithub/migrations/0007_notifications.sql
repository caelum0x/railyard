-- Phase 6: in-app notifications. A notification targets a user about an event in
-- a repo (PR opened/merged, review submitted, issue comment, @mention).

CREATE TABLE IF NOT EXISTS notifications (
    id          UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    user_id     UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    repo_id     UUID REFERENCES repositories(id) ON DELETE CASCADE,
    kind        TEXT NOT NULL,          -- 'pr.opened' | 'pr.merged' | 'review' | 'comment' | 'mention'
    title       TEXT NOT NULL,
    link        TEXT,
    read        BOOLEAN NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS notifications_user_idx ON notifications (user_id, created_at DESC);
