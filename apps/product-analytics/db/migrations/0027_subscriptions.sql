-- Dashboard subscriptions (QY1.4, A25): a dashboard mailed as an HTML table,
-- weekly on a weekday and hour (UTC). Idempotent.

create table if not exists pa.subscriptions (
  id           uuid        primary key default gen_random_uuid(),
  project_id   uuid        not null,
  dashboard_id uuid        not null references pa.dashboards (id) on delete cascade,
  frequency    text        not null default 'weekly' check (frequency in ('weekly')),
  weekday      smallint    not null check (weekday between 0 and 6),
  hour_utc     smallint    not null check (hour_utc between 0 and 23),
  recipients   text[]      not null check (cardinality(recipients) between 1 and 20),
  enabled      boolean     not null default true,
  next_send_at timestamptz not null,
  last_sent_at timestamptz,
  last_error   text,
  created_at   timestamptz not null default now(),
  updated_at   timestamptz not null default now()
);
create index if not exists subscriptions_due_idx on pa.subscriptions (enabled, next_send_at);
