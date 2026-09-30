// Railyard fork addition (PY3.2): retention by property (day or week periods).
import { retention } from '@/railyard/analysis';
import { authorized, db, intParam, json, reply } from '@/railyard/route-kit';

export const dynamic = 'force-dynamic';

export async function GET(request: Request, { params }: { params: Promise<{ org: string }> }) {
  if (!authorized(request)) return json({ error: 'unauthorized' }, 401);
  const q = new URL(request.url).searchParams;
  return reply(
    await retention(
      db,
      (await params).org,
      { app: q.get('app'), period: q.get('period') ?? undefined, periods: intParam(q.get('periods')), days: intParam(q.get('days')), property: q.get('property'), event: q.get('event') },
      new Date(),
    ),
  );
}
