// Railyard fork addition (PY3.2): events or profiles as CSV or JSON (31 days, 100,000 rows at most).
import { exportRows, toCsv } from '@/railyard/export';
import { authorized, db, json } from '@/railyard/route-kit';

export const dynamic = 'force-dynamic';

export async function GET(request: Request, { params }: { params: Promise<{ org: string }> }) {
  if (!authorized(request)) return json({ error: 'unauthorized' }, 401);
  const q = new URL(request.url).searchParams;
  const format = q.get('format') ?? 'json';
  if (format !== 'json' && format !== 'csv') return json({ error: 'format must be json or csv' }, 400);
  const r = await exportRows(db, (await params).org, { kind: q.get('kind') ?? undefined, app: q.get('app'), from: q.get('from'), to: q.get('to') }, new Date());
  if (typeof r === 'string') return json({ error: r }, 400);
  const headers = { 'cache-control': 'no-store', 'x-export-truncated': String(r.truncated) };
  if (format === 'json') return json({ kind: r.kind, truncated: r.truncated, rows: r.rows });
  return new Response(toCsv(r.columns, r.rows), {
    headers: { ...headers, 'content-type': 'text/csv; charset=utf-8', 'content-disposition': `attachment; filename="${r.kind}.csv"` },
  });
}
