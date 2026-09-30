// Railyard fork addition (PY3.3): web vitals p75 per app, per day, and around deploy times.
import { authorized, db, intParam, json, reply } from '@/railyard/route-kit';
import { vitals } from '@/railyard/vitals';

export const dynamic = 'force-dynamic';

export async function GET(request: Request, { params }: { params: Promise<{ org: string }> }) {
  if (!authorized(request)) return json({ error: 'unauthorized' }, 401);
  const q = new URL(request.url).searchParams;
  return reply(await vitals(db, (await params).org, { app: q.get('app'), days: intParam(q.get('days')), marks: q.get('marks') }, new Date()));
}
