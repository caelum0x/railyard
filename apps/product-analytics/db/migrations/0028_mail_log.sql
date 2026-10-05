-- What each alert or subscription mail did, per recipient, and the addresses
-- that unsubscribed (QY1.4). Every mail carries an unsubscribe link; an
-- unsubscribed address is skipped and logged as such. Idempotent.

create table if not exists pa.mail_log (
  id         uuid        primary key default gen_random_uuid(),
  project_id uuid        not null,
  source     text        not null check (source in ('alert', 'subscription')),
  source_id  uuid        not null,
  recipient  text        not null check (length(recipient) between 3 and 320),
  status     text        not null check (status in ('sent', 'failed', 'unsubscribed', 'not_configured')),
  message_id text,
  error      text,
  created_at timestamptz not null default now()
);
create index if not exists mail_log_source_idx on pa.mail_log (project_id, source, source_id, created_at desc);

create table if not exists pa.mail_unsubscribes (
  project_id uuid        not null,
  email      text        not null check (length(email) between 3 and 320),
  created_at timestamptz not null default now(),
  primary key (project_id, email)
);
