-- Destination catalogue (QY1.4, A23): Slack, Discord and HubSpot next to the
-- signed webhook and the S3 bucket. For the new kinds the sealed secret holds
-- the target's own secret (the Slack or Discord webhook address, the HubSpot
-- private app token) and the Standard Webhooks signing secret, together.
-- Idempotent.

alter table pa.export_destinations drop constraint if exists export_destinations_kind_check;
alter table pa.export_destinations add constraint export_destinations_kind_check
  check (kind in ('webhook', 's3', 'slack', 'discord', 'hubspot'));
