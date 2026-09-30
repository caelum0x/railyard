-- Surveys (A3). Answers are not stored here: they arrive as `survey sent`
-- events in pa.events, so a person's deletion removes them with the events.
-- Idempotent. Row level security in 0024.

create table if not exists pa.surveys (
  id             uuid        primary key default gen_random_uuid(),
  project_id     uuid        not null,
  name           text        not null check (length(name) between 1 and 200),
  description    text        not null default '' check (length(description) <= 1000),
  type           text        not null default 'popover' check (type in ('popover', 'api')),
  questions      jsonb       not null check (jsonb_typeof(questions) = 'array'),
  appearance     jsonb       not null default '{}'::jsonb check (jsonb_typeof(appearance) = 'object'),
  url            text        check (length(url) between 1 and 500),
  url_match      text        not null default 'icontains' check (url_match in ('icontains', 'exact', 'regex')),
  flag_key       text        check (flag_key ~ '^[a-z][a-z0-9_]{0,63}$'),
  cohort_id      uuid,
  status         text        not null default 'draft' check (status in ('draft', 'running', 'stopped')),
  start_date     timestamptz,
  end_date       timestamptz,
  response_limit integer     check (response_limit between 1 and 1000000),
  created_at     timestamptz not null default now(),
  updated_at     timestamptz not null default now(),
  unique (project_id, name)
);
create index if not exists surveys_project_status_idx on pa.surveys (project_id, status);
