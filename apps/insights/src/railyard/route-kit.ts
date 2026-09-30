// Railyard fork addition: shared glue for the tenant API routes (PY3.2).
// Bearer INSIGHTS_API_TOKEN (or _NEXT while rotating); the Railyard console is
// the only caller and always names the org it acts for.
import prisma from '@/lib/prisma';
import { bearerOk } from '@/railyard/ingest';

export const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json', 'cache-control': 'no-store' } });

export function authorized(request: Request): boolean {
  return bearerOk(request.headers.get('authorization'), [process.env.INSIGHTS_API_TOKEN, process.env.INSIGHTS_API_TOKEN_NEXT]);
}

/** The upstream raw query client, typed for the fork modules. */
export const db = {
  query: async <T>(sql: string, data: Record<string, unknown>): Promise<T[]> => (await prisma.rawQuery(sql, data)) as T[],
};
export const writeDb = {
  query: async <T>(sql: string, data: Record<string, unknown>): Promise<T[]> => {
    const rows = await prisma.writeRawQuery(sql, data);
    return (Array.isArray(rows) ? rows : []) as T[];
  },
};

/** A result that is a string is a request error (400); anything else is the body. */
export const reply = (r: unknown) => (typeof r === 'string' ? json({ error: r }, 400) : json(r));

export const intParam = (v: string | null) => (v === null || v === '' ? undefined : Number(v));
