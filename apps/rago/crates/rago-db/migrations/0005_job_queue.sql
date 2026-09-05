-- Durable, Postgres-backed job queue (replaces in-memory scheduling).
CREATE TABLE IF NOT EXISTS jobs (
    id uuid DEFAULT gen_random_uuid() NOT NULL PRIMARY KEY,
    queue varchar DEFAULT 'default' NOT NULL,
    job_type varchar NOT NULL,
    payload jsonb DEFAULT '{}' NOT NULL,
    status varchar DEFAULT 'pending' NOT NULL, -- pending | leased | succeeded | failed | dead
    attempts integer DEFAULT 0 NOT NULL,
    max_attempts integer DEFAULT 5 NOT NULL,
    run_at timestamp(6) DEFAULT now() NOT NULL,
    leased_at timestamp(6),
    leased_by varchar,
    last_error text,
    created_at timestamp(6) DEFAULT now() NOT NULL,
    updated_at timestamp(6) DEFAULT now() NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_jobs_due ON jobs (status, run_at);

-- Webhook re-delivery scheduling: when the next retry is due.
ALTER TABLE webhooks ADD COLUMN IF NOT EXISTS next_retry_at timestamp(6);
