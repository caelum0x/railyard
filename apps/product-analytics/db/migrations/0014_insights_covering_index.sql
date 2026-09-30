-- Insights (A2): person-level insights (unique people, retention, lifecycle,
-- stickiness, cohorts) read (event, timestamp, person_id) only, so this
-- covering index lets them skip the table (index-only scans). Measured in
-- docs/analytics/CAPACITY.md. It makes events_project_event_ts_idx (0002)
-- redundant; dropping that one is left to its owner (hand-off in oss/A2.md).
create index if not exists events_project_event_ts_person_idx on pa.events (project_id, event, "timestamp") include (person_id);
