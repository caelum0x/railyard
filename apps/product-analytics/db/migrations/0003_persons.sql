-- People: a person row, its distinct ids, and merge overrides.
-- Merges never rewrite stored events: queries read
-- coalesce(o.new_person_id, e.person_id) through pa.person_overrides.

create table if not exists pa.persons (
  id            uuid        primary key default gen_random_uuid(),
  project_id    uuid        not null,
  properties    jsonb       not null default '{}'::jsonb,
  is_identified boolean     not null default false,
  created_at    timestamptz not null default now(),
  version       bigint      not null default 0
);
create index if not exists persons_project_created_idx on pa.persons (project_id, created_at desc);

create table if not exists pa.person_distinct_ids (
  project_id  uuid   not null,
  distinct_id text   not null check (length(distinct_id) between 1 and 200),
  person_id   uuid   not null references pa.persons (id) on delete cascade,
  version     bigint not null default 0,
  primary key (project_id, distinct_id)
);
create index if not exists person_distinct_ids_person_idx on pa.person_distinct_ids (person_id);

create table if not exists pa.person_overrides (
  project_id    uuid        not null,
  old_person_id uuid        not null,
  new_person_id uuid        not null,
  created_at    timestamptz not null default now(),
  primary key (project_id, old_person_id),
  check (old_person_id <> new_person_id)
);
create index if not exists person_overrides_new_idx on pa.person_overrides (project_id, new_person_id);
