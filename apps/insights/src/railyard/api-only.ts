/**
 * INSIGHTS_API_ONLY=1 (fork addition): the upstream dashboard, login and
 * tracker are never served from this deployment. Only the ingest and tenant
 * API (behind their bearer tokens) and the heartbeat answer; everything else
 * is 404. Customers see Insights only through the Railyard console and the
 * Scribase Analytics tab.
 */

const ALLOWED = [/^\/v1\//, /^\/api\/v1\//, /^\/api\/heartbeat$/];

export function allowedPath(pathname: string, apiOnly: boolean): boolean {
  if (!apiOnly) return true;
  return ALLOWED.some((re) => re.test(pathname));
}
