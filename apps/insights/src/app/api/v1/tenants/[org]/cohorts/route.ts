// Railyard fork addition (PY3.2): saved cohorts with live member counts; POST saves (by name).
import { uuid } from '@/lib/crypto';
import { listCohorts, saveCohort } from '@/railyard/cohorts';
import { authorized, db, json, reply, writeDb } from '@/railyard/route-kit';

export const dynamic = 'force-dynamic';

export async function GET(request: Request, { params }: { params: Promise<{ org: string }> }) {
  if (!authorized(request)) return json({ error: 'unauthorized' }, 401);
  return reply(await listCohorts(db, (await params).org, new Date()));
}

export async function POST(request: Request, { params }: { params: Promise<{ org: string }> }) {
  if (!authorized(request)) return json({ error: 'unauthorized' }, 401);
  return reply(await saveCohort(writeDb, (await params).org, await request.json().catch(() => null), new Date(), () => uuid()));
}
