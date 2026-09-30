// Railyard fork addition (PY3.2): delete one person by the app's user id (done at once; retried within 24 h).
import { uuid } from '@/lib/crypto';
import { eraseUser } from '@/railyard/people';
import { authorized, json, writeDb } from '@/railyard/route-kit';

export const dynamic = 'force-dynamic';

export async function DELETE(request: Request, { params }: { params: Promise<{ org: string; user: string }> }) {
  if (!authorized(request)) return json({ error: 'unauthorized' }, 401);
  const { org, user } = await params;
  const r = await eraseUser(writeDb, org, decodeURIComponent(user), new URL(request.url).searchParams.get('app'), new Date(), () => uuid());
  if (typeof r === 'string') return json({ error: r }, 400);
  return json(r, r.status === 'done' ? 200 : 202);
}
