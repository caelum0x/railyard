-- Rollups (A2): the rollup job finds the hours touched by newly inserted
-- events, and trends count the not-yet-rolled tail, through this index.
create index if not exists events_project_inserted_idx on pa.events (project_id, inserted_at);
