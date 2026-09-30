-- Ingest bookkeeping: receipts (a retried batch is stored once), monthly
-- usage per project (the product's own count), and the first stored event.

create table if not exists pa.ingest_receipts (
  project_id  uuid        not null,
  uuid        uuid        not null,
  received_at timestamptz not null default now(),
  primary key (project_id, uuid)
);
create index if not exists ingest_receipts_received_idx on pa.ingest_receipts (received_at);

create table if not exists pa.usage_monthly (
  project_id uuid   not null,
  org_id     uuid   not null,
  month      date   not null check (extract(day from month) = 1),
  received   bigint not null default 0 check (received >= 0),
  stored     bigint not null default 0 check (stored >= 0),
  capped     bigint not null default 0 check (capped >= 0),
  dropped    bigint not null default 0 check (dropped >= 0),
  primary key (project_id, month)
);
create index if not exists usage_monthly_org_idx on pa.usage_monthly (org_id, month);

create table if not exists pa.project_firsts (
  project_id uuid        primary key,
  org_id     uuid        not null,
  event_uuid uuid        not null,
  event      text        not null,
  at         timestamptz not null
);
create index if not exists project_firsts_org_idx on pa.project_firsts (org_id);
