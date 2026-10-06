-- Warehouse destinations (A22): ClickHouse and BigQuery tables of the
-- customer's own, next to the S3 file export. The sealed secret holds the
-- ClickHouse password or the BigQuery service account key, with the signing
-- secret. Idempotent.

alter table pa.export_destinations drop constraint if exists export_destinations_kind_check;
alter table pa.export_destinations add constraint export_destinations_kind_check
  check (kind in ('webhook', 's3', 'slack', 'discord', 'hubspot', 'clickhouse', 'bigquery'));
