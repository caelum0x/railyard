// Railyard fork addition: one org's summary for the Railyard console and the
// Scribase Analytics tab (src/railyard/summary.ts). Bearer INSIGHTS_API_TOKEN.
import prisma from '@/lib/prisma';
import { bearerOk } from '@/railyard/ingest';
import { summarize } from '@/railyard/summary';

export const dynamic = 'force-dynamic';

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json', 'cache-control': 'no-store' } });

export async function GET(request: Request, { params }: { params: Promise<{ org: string }> }) {
  if (!bearerOk(request.headers.get('authorization'), [process.env.INSIGHTS_API_TOKEN, process.env.INSIGHTS_API_TOKEN_NEXT])) {
    return json({ error: 'unauthorized' }, 401);
  }
  const { org } = await params;
  const url = new URL(request.url);
  const days = Number(url.searchParams.get('days') ?? '7');
  const result = await summarize(
    { query: async (sql, data) => (await prisma.rawQuery(sql, data)) as never },
    org,
    url.searchParams.get('app'),
    days,
    new Date(),
  );
  return typeof result === 'string' ? json({ error: result }, 400) : json(result);
}
