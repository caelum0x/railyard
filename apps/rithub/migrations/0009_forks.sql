-- Phase 7: forks + cross-repo pull requests. A PR may originate from a fork:
-- `head_repo_id` records the source repo (NULL = same-repo PR), and the PR head
-- is pinned to `head_sha` (imported into the base repo as refs/pull/<n>/head).

ALTER TABLE pull_requests
    ADD COLUMN IF NOT EXISTS head_repo_id UUID REFERENCES repositories(id) ON DELETE SET NULL;

-- Record a fork relationship for listing/provenance.
ALTER TABLE repositories
    ADD COLUMN IF NOT EXISTS forked_from UUID REFERENCES repositories(id) ON DELETE SET NULL;
