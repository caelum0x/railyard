// Railyard fork addition (PY3.2): events and visitors broken down by one dimension.
import { breakdown } from '@/railyard/analysis';
import { authorized, db, intParam, json, reply } from '@/railyard/route-kit';

export const dynamic = 'force-dynamic';

export async function GET(request: Request, { params }: { params: Promise<{ org: string }> }) {
  if (!authorized(request)) return json({ error: 'unauthorized' }, 401);
  const q = new URL(request.url).searchParams;
  return reply(
    await breakdown(
      db,
      (await params).org,
      { app: q.get('app'), dimension: q.get('dimension') ?? '', days: intParam(q.get('days')), event: q.get('event'), limit: intParam(q.get('limit')) },
      new Date(),
    ),
  );
}
