-- Insights (A2): which people did each event on each UTC day, from events
-- inserted before the project's rollup watermark (pa.rollup_state). Unique
-- people per day, week or month read these rows instead of every event;
-- person ids are resolved through pa.person_overrides at query time.
create table if not exists pa.rollup_daily_people (
  project_id uuid not null,
  event      text not null,
  day        date not null,
  person_id  uuid not null,
  primary key (project_id, event, day, person_id)
);

alter table pa.rollup_daily_people enable row level security;
drop policy if exists project_isolation on pa.rollup_daily_people;
create policy project_isolation on pa.rollup_daily_people for all to pa_app
  using (project_id = pa.current_project()) with check (project_id = pa.current_project());
grant select, insert, update, delete on pa.rollup_daily_people to pa_app;
