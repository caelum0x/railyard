-- Group analytics: up to five group types per project, each with keyed groups.

create table if not exists pa.group_types (
  project_id       uuid        not null,
  group_type       text        not null check (length(group_type) between 1 and 100),
  group_type_index smallint    not null check (group_type_index between 0 and 4),
  created_at       timestamptz not null default now(),
  primary key (project_id, group_type),
  unique (project_id, group_type_index)
);

create table if not exists pa.groups (
  project_id       uuid        not null,
  group_type_index smallint    not null check (group_type_index between 0 and 4),
  group_key        text        not null check (length(group_key) between 1 and 400),
  properties       jsonb       not null default '{}'::jsonb,
  created_at       timestamptz not null default now(),
  updated_at       timestamptz not null default now(),
  version          bigint      not null default 0,
  primary key (project_id, group_type_index, group_key)
);
