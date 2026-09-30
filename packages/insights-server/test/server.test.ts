// @railyard/insights-server (Wave P PY3.2) against the edge collector's own
// parser and the console's real forwarding code (lib/insights-depth/forward.ts)
// with a stand-in Insights behind it.
//   node --test test/*.test.ts
import { test } from 'node:test';
import assert from 'node:assert/strict';

import { createInsightsServer, InsightsError } from '../src/index.ts';
import { parseOne } from '../../../apps/insights-edge/src/payload.mjs';
import { MemoryRepo } from '../../../apps/console/lib/repo.ts';
import { emptyEntitlement } from '../../../apps/console/lib/types.ts';
import { sha256Hex } from '../../../apps/console/lib/crypto.ts';
import { forwardInsights } from '../../../apps/console/lib/insights-depth/forward.ts';

const SITE = 'ry_site_server123456';
const TOKEN = `ry_${'s'.repeat(30)}`;

async function world() {
  const repo = new MemoryRepo();
  const org = await repo.createOrgForUser({ userId: 'u1', email: 'a@example.com', name: 'Acme' });
  await repo.saveEntitlement({ ...emptyEntitlement(org.id, new Date()), plan: 'addon', status: 'active', addon_slots: ['analytics'], apps_limit: 5 });
  const app = await repo.createApp({ orgId: org.id, name: 'shop', siteKey: SITE });
  await repo.createToken({ org_id: org.id, user_id: 'u1', token_hash: await sha256Hex(TOKEN), label: 'server' });
  const collected: unknown[] = [];
  const insightsCalls: string[] = [];
  let collectorFails = 0;
  const insightsFetch = (async (url: string, init: RequestInit) => {
    insightsCalls.push(`${init.method} ${url}`);
    if (url.includes('/export') && url.includes('format=csv')) return new Response('app_id,user_id\r\n', { headers: { 'content-type': 'text/csv' } });
    if (url.includes('/users/')) return new Response(JSON.stringify({ request_id: 'r1', status: 'done', rows_deleted: 9, due_by: 'x' }));
    return new Response(JSON.stringify({ rows: [], cohorts: [], profiles: [], cohort_id: 'c1' }));
  }) as unknown as typeof fetch;
  const fetch = async (url: string, init?: { method?: string; headers?: Record<string, string>; body?: string }) => {
    if (url.startsWith('https://collect.test/')) {
      if (collectorFails > 0) {
        collectorFails -= 1;
        return new Response('', { status: 503 });
      }
      for (const item of JSON.parse(init?.body ?? '[]')) {
        const p = parseOne(item);
        assert.ok(!('error' in p), JSON.stringify(p));
        collected.push(item);
      }
      return new Response('{}', { status: 202 });
    }
    // The console: the real forwarding code.
    const req = new Request(url, { method: init?.method, headers: init?.headers, body: init?.body });
    const path = new URL(url).pathname.replace('/api/v1/insights/', '').split('/').map(decodeURIComponent);
    return forwardInsights({ repo, insightsApi: { url: 'https://insights.internal', token: 'platform-token-xyz-0123456789' }, live: true, fetchImpl: insightsFetch }, req, path, new Date());
  };
  return { org, app, collected, insightsCalls, fetch: fetch as never, failCollector: (n: number) => (collectorFails = n) };
}

test('identify and track from a server name the person and pass the collector parser; a 5xx is retried', async () => {
  const w = await world();
  const i = createInsightsServer({ site: SITE, collector: 'https://collect.test', fetch: w.fetch });
  await i.identify('user_42', { plan: 'pro', nested: { x: 1 } });
  w.failCollector(2);
  await i.track('user_42', 'invoice_paid', { amount: 49 });
  const [id, ev] = w.collected as { type: string; payload: Record<string, unknown> }[];
  assert.deepEqual([id!.type, id!.payload.user, id!.payload.data], ['identify', 'user_42', { plan: 'pro' }]);
  assert.deepEqual([ev!.type, ev!.payload.user, ev!.payload.name, ev!.payload.data], ['event', 'user_42', 'invoice_paid', { amount: 49 }]);
  w.failCollector(3);
  await assert.rejects(i.track('user_42', 'x'), (e: InsightsError) => e.status === 503);
  await assert.rejects(i.track('not valid', 'x'), /user id/);
  await assert.rejects(createInsightsServer({ fetch: w.fetch }).track('u', 'x'), /site/);
});

test('reads, exports and deletes go through the console with the org token; app names resolve to the org\'s app', async () => {
  const w = await world();
  const i = createInsightsServer({ token: TOKEN, console: 'https://railyard.test', app: 'shop', fetch: w.fetch });
  await i.cohorts.save('Active pro', { performed: { event: 'created_project', min_times: 2, within_days: 30 } });
  await i.retention({ property: 'trait.plan', period: 'week' });
  const csv = await i.exportEvents({ format: 'csv' });
  assert.equal(csv, 'app_id,user_id\r\n');
  const del = await i.deleteUser('user@42');
  assert.equal(del.status, 'done');
  assert.deepEqual(w.insightsCalls, [
    `POST https://insights.internal/api/v1/tenants/${w.org.id}/cohorts?app=${w.app.id}`,
    `GET https://insights.internal/api/v1/tenants/${w.org.id}/retention?period=week&property=trait.plan&app=${w.app.id}`,
    `GET https://insights.internal/api/v1/tenants/${w.org.id}/export?kind=events&format=csv&app=${w.app.id}`,
    `DELETE https://insights.internal/api/v1/tenants/${w.org.id}/users/user%4042?app=${w.app.id}`,
  ]);
  await assert.rejects(createInsightsServer({ token: `ry_${'x'.repeat(30)}`, console: 'https://railyard.test', fetch: w.fetch }).profiles(), (e: InsightsError) => e.status === 401);
  await assert.rejects(createInsightsServer({ token: TOKEN, console: 'https://railyard.test', app: 'someone-elses', fetch: w.fetch }).profiles(), (e: InsightsError) => e.status === 404);
  await assert.rejects(createInsightsServer({ console: 'https://railyard.test', fetch: w.fetch }).profiles(), /Set token/);
});
