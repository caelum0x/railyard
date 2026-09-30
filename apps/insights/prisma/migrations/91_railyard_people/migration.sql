-- Railyard fork addition (PY3.2, Insights depth): identified people, saved
-- cohorts and delete-by-user requests. Idempotent; applied only to scratch
-- Postgres by agents (the image's migrate step applies it on box 2 at deploy).
--
-- railyard_profile: one row per (app, user id) from `identify`: first and last
-- seen, and the latest traits (a later identify merges its traits in).
CREATE TABLE IF NOT EXISTS "railyard_profile" (
    "website_id" UUID NOT NULL,
    "distinct_id" VARCHAR(50) NOT NULL,
    "first_seen" TIMESTAMPTZ(6) NOT NULL,
    "last_seen" TIMESTAMPTZ(6) NOT NULL,
    "traits" JSONB NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT "railyard_profile_pkey" PRIMARY KEY ("website_id", "distinct_id"),
    CONSTRAINT "railyard_profile_traits_object" CHECK (jsonb_typeof("traits") = 'object')
);
CREATE INDEX IF NOT EXISTS "railyard_profile_website_last_seen_idx" ON "railyard_profile" ("website_id", "last_seen" DESC);

-- Saved cohorts per org (team). The definition is validated by the fork
-- (src/railyard/cohorts.ts) before it is stored; SQL is built from it with
-- bound parameters only.
CREATE TABLE IF NOT EXISTS "railyard_cohort" (
    "cohort_id" UUID NOT NULL,
    "team_id" UUID NOT NULL,
    "name" VARCHAR(100) NOT NULL,
    "definition" JSONB NOT NULL,
    "created_at" TIMESTAMPTZ(6) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "updated_at" TIMESTAMPTZ(6) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CONSTRAINT "railyard_cohort_pkey" PRIMARY KEY ("cohort_id"),
    CONSTRAINT "railyard_cohort_team_name_key" UNIQUE ("team_id", "name")
);

-- Delete-by-user requests: done at once when possible; a pending row is retried
-- by the sweep (POST /api/v1/erasure/sweep, hourly) until done, within 24 h.
CREATE TABLE IF NOT EXISTS "railyard_erasure" (
    "request_id" UUID NOT NULL,
    "team_id" UUID NOT NULL,
    "website_id" UUID,
    "distinct_id" VARCHAR(50) NOT NULL,
    "requested_at" TIMESTAMPTZ(6) NOT NULL DEFAULT CURRENT_TIMESTAMP,
    "done_at" TIMESTAMPTZ(6),
    "rows_deleted" INTEGER,
    CONSTRAINT "railyard_erasure_pkey" PRIMARY KEY ("request_id")
);
CREATE INDEX IF NOT EXISTS "railyard_erasure_pending_idx" ON "railyard_erasure" ("requested_at") WHERE "done_at" IS NULL;
