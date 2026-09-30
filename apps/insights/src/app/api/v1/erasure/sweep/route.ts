// Railyard fork addition (PY3.2): retries pending delete-by-user requests (hourly timer on box 2).
import { sweepErasures } from '@/railyard/people';
import { authorized, json, writeDb } from '@/railyard/route-kit';

export const dynamic = 'force-dynamic';

export async function POST(request: Request) {
  if (!authorized(request)) return json({ error: 'unauthorized' }, 401);
  return json(await sweepErasures(writeDb));
}
