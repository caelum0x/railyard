-- Console cross-sell bridge (RYP-X1): first-touch signup attribution.
-- Records where a new org's owner first clicked through from (e.g.
-- "insights" for the Insights dashboard banner), written once at org
-- creation by lib/signin.ts via lib/session.ts's SRC_COOKIE. Idempotent.

alter table railyard.orgs
  add column if not exists signup_source text;

alter table railyard.orgs
  drop constraint if exists orgs_signup_source_format;
alter table railyard.orgs
  add constraint orgs_signup_source_format
  check (signup_source is null or signup_source ~ '^[a-z0-9_-]{1,40}$');

create index if not exists orgs_signup_source_idx on railyard.orgs (signup_source) where signup_source is not null;
