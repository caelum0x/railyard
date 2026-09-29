// Railyard fork addition: batch ingest from the edge collector (src/railyard/ingest.ts).
import { uuid } from '@/lib/crypto';
import prisma from '@/lib/prisma';
import { createSession, saveEvent } from '@/queries/sql';
import { bearerOk, ingest, parseIngest } from '@/railyard/ingest';
import { ENSURE_TEAM_SQL, ENSURE_WEBSITE_SQL, RECEIPT_SQL, WEBSITE_OF_ORG_SQL } from '@/railyard/tenancy';

export const dynamic = 'force-dynamic';

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json', 'cache-control': 'no-store' } });

export async function POST(request: Request) {
  if (!bearerOk(request.headers.get('authorization'), [process.env.INSIGHTS_INGEST_TOKEN, process.env.INSIGHTS_INGEST_TOKEN_NEXT])) {
    return json({ error: 'unauthorized' }, 401);
  }
  const parsed = parseIngest(await request.json().catch(() => null));
  if (typeof parsed === 'string') return json({ error: parsed }, 400);
  const result = await ingest(
    {
      receipt: async (orgId, sentAt, siteKey) => {
        const rows = await prisma.writeRawQuery(RECEIPT_SQL, { orgId, sentAt, siteKey });
        return Array.isArray(rows) && rows.length === 1;
      },
      ensureWebsite: async (orgId, appId, siteKey, hostname) => {
        await prisma.writeRawQuery(ENSURE_TEAM_SQL, { orgId, name: `org ${orgId.slice(0, 8)}` });
        await prisma.writeRawQuery(ENSURE_WEBSITE_SQL, { appId, orgId, name: siteKey.slice(0, 100), domain: hostname });
        const owned = await prisma.rawQuery(WEBSITE_OF_ORG_SQL, { appId, orgId });
        return Array.isArray(owned) && owned.length === 1;
      },
      createSession: async (row) => {
        await createSession({ ...row, browser: null, os: null, device: null, region: null, city: null, distinctId: null } as never);
      },
      saveEvent: async (row) => {
        await saveEvent(row as never);
      },
      id: (...parts) => uuid(...parts),
    },
    parsed,
  );
  return json(result.body, result.status);
}
