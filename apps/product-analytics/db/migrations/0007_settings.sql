-- Per-project settings the console edits and remote config reads.

create table if not exists pa.project_settings (
  project_id       uuid        primary key,
  org_id           uuid        not null,
  require_consent  boolean     not null default true,
  autocapture      boolean     not null default true,
  capture_pageview boolean     not null default true,
  flags_client_key text        check (flags_client_key ~ '^ry_flags_[A-Za-z0-9_-]{24,64}$'),
  retention_days   integer     check (retention_days between 1 and 3650),
  updated_at       timestamptz not null default now()
);
