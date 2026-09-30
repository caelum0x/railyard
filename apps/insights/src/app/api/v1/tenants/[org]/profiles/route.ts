// Railyard fork addition (PY3.2): identified people of one org, or one person with recent events.
import { authorized, db, intParam, json, reply } from '@/railyard/route-kit';
import { listProfiles, profileEvents } from '@/railyard/people';

export const dynamic = 'force-dynamic';

export async function GET(request: Request, { params }: { params: Promise<{ org: string }> }) {
  if (!authorized(request)) return json({ error: 'unauthorized' }, 401);
  const { org } = await params;
  const q = new URL(request.url).searchParams;
  const user = q.get('user');
  const r = await listProfiles(db, org, { app: q.get('app'), user, limit: intParam(q.get('limit')), offset: intParam(q.get('offset')) });
  if (typeof r === 'string' || !user) return reply(r);
  return json({ ...r, events: r.profiles.length ? await profileEvents(db, org, user, q.get('app')) : [] });
}
