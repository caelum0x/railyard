-- Phase D1: search index metadata + a Postgres full-text fallback store.
--
-- The PRIMARY search backend is the on-disk Tantivy index owned by
-- `forge-search` (repos, issues, PRs, commit messages, and blob content, with
-- BM25 ranking). This migration adds:
--
--   1. `search_index_meta` — a singleton checkpoint row recording the active
--      backend, the last full-reindex time, and the document count. The
--      `reindex` command writes it so operators can see index freshness.
--
--   2. `search_documents` — a Postgres `tsvector` mirror used when the forge is
--      run with `RITHUB_SEARCH_BACKEND=postgres` (or Tantivy is unavailable in a
--      given build). It is populated by the same upsert path as the Tantivy
--      index and by `reindex`, so the fallback returns fresh, ranked results
--      without depending on the native index. `ON DELETE CASCADE` ties document
--      cleanup to repository deletion automatically.
--
-- Append-only migration: never edit in place; add a new numbered file instead.

CREATE TABLE IF NOT EXISTS search_index_meta (
    -- Enforced singleton: exactly one metadata row.
    id              SMALLINT    PRIMARY KEY DEFAULT 1 CHECK (id = 1),
    backend         TEXT        NOT NULL DEFAULT 'tantivy',
    last_reindex_at TIMESTAMPTZ,
    document_count  BIGINT      NOT NULL DEFAULT 0,
    updated_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS search_documents (
    -- Stable, kind-prefixed key (mirrors the Tantivy `doc_id`), e.g.
    -- 'issue:<repo_id>:<number>', 'blob:<repo_id>:<path>'. Re-indexing upserts.
    doc_id      TEXT        PRIMARY KEY,
    kind        TEXT        NOT NULL,          -- repo|issue|pull_request|commit|blob
    repo_id     UUID        NOT NULL REFERENCES repositories(id) ON DELETE CASCADE,
    org         TEXT        NOT NULL,
    repo        TEXT        NOT NULL,
    visibility  TEXT        NOT NULL,          -- 'public' | 'private'
    number      INTEGER     NOT NULL DEFAULT 0,
    oid         TEXT        NOT NULL DEFAULT '',
    path        TEXT        NOT NULL DEFAULT '',
    title       TEXT        NOT NULL DEFAULT '',
    body        TEXT        NOT NULL DEFAULT '',
    -- Weighted, language-neutral ('simple') full-text vector: title ranks above
    -- body. The 2-arg `to_tsvector('simple', …)` form is IMMUTABLE, so it is
    -- valid inside a generated column.
    tsv         tsvector    GENERATED ALWAYS AS (
                    setweight(to_tsvector('simple', coalesce(title, '')), 'A') ||
                    setweight(to_tsvector('simple', coalesce(body, '')), 'B')
                ) STORED,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Full-text lookups (the hot path for the Postgres backend).
CREATE INDEX IF NOT EXISTS search_documents_tsv_idx
    ON search_documents USING GIN (tsv);
-- Permission scoping filters by repo id and visibility.
CREATE INDEX IF NOT EXISTS search_documents_repo_idx
    ON search_documents (repo_id);
CREATE INDEX IF NOT EXISTS search_documents_visibility_idx
    ON search_documents (visibility);
CREATE INDEX IF NOT EXISTS search_documents_kind_idx
    ON search_documents (kind);
