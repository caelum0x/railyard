-- Privacy jobs: one row per person deletion, visible in the console until done.

create table if not exists pa.gdpr_jobs (
  id           uuid        primary key default gen_random_uuid(),
  project_id   uuid        not null,
  kind         text        not null default 'delete' check (kind in ('delete')),
  distinct_id  text        not null check (length(distinct_id) between 1 and 200),
  requested_by text        not null check (length(requested_by) between 1 and 200),
  requested_at timestamptz not null,
  due_at       timestamptz not null,
  status       text        not null default 'queued' check (status in ('queued', 'running', 'done', 'failed')),
  started_at   timestamptz,
  finished_at  timestamptz,
  attempts     integer     not null default 0,
  result       jsonb,
  error        text,
  check (due_at > requested_at)
);
create index if not exists gdpr_jobs_status_idx on pa.gdpr_jobs (status, due_at);
create index if not exists gdpr_jobs_project_idx on pa.gdpr_jobs (project_id, requested_at desc);
