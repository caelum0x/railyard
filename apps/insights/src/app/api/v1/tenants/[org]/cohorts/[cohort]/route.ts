// Railyard fork addition (PY3.2): one cohort's members; DELETE removes the saved cohort (not the people).
import { cohortMembers, DELETE_COHORT_SQL } from '@/railyard/cohorts';
import { authorized, db, intParam, json, reply, writeDb } from '@/railyard/route-kit';

export const dynamic = 'force-dynamic';

export async function GET(request: Request, { params }: { params: Promise<{ org: string; cohort: string }> }) {
  if (!authorized(request)) return json({ error: 'unauthorized' }, 401);
  const { org, cohort } = await params;
  const limit = intParam(new URL(request.url).searchParams.get('limit')) ?? 1000;
  if (!Number.isInteger(limit) || limit < 1 || limit > 10_000) return json({ error: 'limit must be 1 to 10000' }, 400);
  const r = await cohortMembers(db, org, cohort, new Date(), limit);
  return r === null ? json({ error: 'not found' }, 404) : reply(r);
}

export async function DELETE(request: Request, { params }: { params: Promise<{ org: string; cohort: string }> }) {
  if (!authorized(request)) return json({ error: 'unauthorized' }, 401);
  const { org, cohort } = await params;
  const uuidRe = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  if (!uuidRe.test(org) || !uuidRe.test(cohort)) return json({ error: 'org and cohort must be UUIDs' }, 400);
  const rows = await writeDb.query(DELETE_COHORT_SQL, { orgId: org, cohortId: cohort });
  return rows.length ? json({ deleted: true }) : json({ error: 'not found' }, 404);
}
