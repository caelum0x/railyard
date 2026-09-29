/**
 * Railyard tenancy on top of the upstream schema (fork addition).
 *
 * One upstream team per Railyard org (team_id = the org's UUID) and one
 * website per Railyard app (website_id = the app's UUID, team_id = its org).
 * Every tenant query names the org and joins through `website.team_id`, so an
 * org can only ever read its own apps' rows.
 */

export const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function isUuid(value: unknown): value is string {
  return typeof value === 'string' && UUID.test(value);
}

// SQL in the upstream raw-query dialect: `{{name}}` or `{{name::type}}`
// placeholders become bound parameters (never string interpolation).

/** Makes sure the org's team exists. */
export const ENSURE_TEAM_SQL =
  `insert into team (team_id, name, created_at, updated_at) values ({{orgId::uuid}}, {{name}}, now(), now()) ` +
  `on conflict (team_id) do nothing`;

/** Makes sure the app's website exists under its org. */
export const ENSURE_WEBSITE_SQL =
  `insert into website (website_id, name, domain, team_id, created_at, updated_at) ` +
  `values ({{appId::uuid}}, {{name}}, {{domain}}, {{orgId::uuid}}, now(), now()) ` +
  `on conflict (website_id) do nothing`;

/** The app's website only if it belongs to the org (a website id can never move orgs). */
export const WEBSITE_OF_ORG_SQL =
  `select website_id from website where website_id = {{appId::uuid}} and team_id = {{orgId::uuid}} and deleted_at is null`;

/** De-duplicates retried collector batches (migration 90_railyard_ingest_receipts). */
export const RECEIPT_SQL =
  `insert into railyard_ingest_receipt (org_id, sent_at, site_key) values ({{orgId::uuid}}, {{sentAt}}, {{siteKey}}) ` +
  `on conflict do nothing returning org_id`;

/** Converts the placeholder dialect to `$n` for a plain Postgres client (tests, scripts). */
export function toPositional(sql: string, data: Record<string, unknown>): { text: string; values: unknown[] } {
  const values: unknown[] = [];
  const text = sql.replace(/\{\{\s*(\w+)(::\w+)?\s*\}\}/g, (_m, name: string, type?: string) => {
    values.push(data[name]);
    return `$${values.length}${type ?? ''}`;
  });
  return { text, values };
}
