// The unmodified server SDK (directly and through @railyard/analytics-node)
// against the local service: capture with groups, identify, alias, group
// properties and flag checks all land with the right fields.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { PostHog } from "posthog-node";
import { MemoryFlagsReader } from "../../src/flags/client.ts";
import { putSettings, DEFAULT_SETTINGS } from "../../src/tenancy/settings.ts";
import { count, makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "./helpers.ts";

const KEY = "ry_flags_nodenodenodenodenodenodenode01";
let pg: ScratchPg;
let run: Running;
const site = makeSite();

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site], { flags: new MemoryFlagsReader({ [KEY]: { new_checkout: { enabled: true, value: "blue" }, off_flag: { enabled: false, value: null } } }) });
  await putSettings(run.svc.deps.db, site, { ...DEFAULT_SETTINGS, flags_client_key: KEY });
});

test("posthog-node (unmodified, gzip on) stores capture, identify, alias and groups", { skip: skipNoPg }, async () => {
  const ph = new PostHog(site.token, { host: run.base, flushAt: 100, flushInterval: 0 });
  ph.capture({ distinctId: "srv-1", event: "invoice_paid", properties: { amount_cents: 1900 }, groups: { company: "acme" } });
  ph.identify({ distinctId: "srv-1", properties: { $set: { plan: "pro" }, $set_once: { first_plan: "free" } } });
  ph.alias({ distinctId: "srv-1", alias: "billing-1" });
  ph.groupIdentify({ groupType: "company", groupKey: "acme", properties: { name: "Acme" } });
  await ph.shutdown();
  await run.svc.deps.writer.flush();
  assert.equal(count(pg, `project_id = '${site.project_id}' and event = 'invoice_paid' and properties ->> 'amount_cents' = '1900' and "$group_0" = 'acme' and properties ->> '$lib' = 'posthog-node'`), 1);
  const p = pg.psql(`select person_id from pa.person_distinct_ids where project_id = '${site.project_id}' and distinct_id = 'srv-1'`);
  assert.equal(pg.psql(`select person_id from pa.person_distinct_ids where project_id = '${site.project_id}' and distinct_id = 'billing-1'`), p);
  assert.equal(pg.psql(`select (properties ->> 'plan') || '|' || (properties ->> 'first_plan') from pa.persons where id = '${p}'`), "pro|free");
  assert.equal(pg.psql(`select properties ->> 'name' from pa.groups where project_id = '${site.project_id}' and group_key = 'acme'`), "Acme");
});

test("posthog-node flag checks read the flags slot through the service", { skip: skipNoPg }, async () => {
  const ph = new PostHog(site.token, { host: run.base, flushAt: 100, flushInterval: 0 });
  assert.equal(await ph.isFeatureEnabled("new_checkout", "srv-2"), true);
  assert.equal(await ph.isFeatureEnabled("off_flag", "srv-2"), false);
  assert.equal(await ph.getFeatureFlagPayload("new_checkout", "srv-2"), "blue");
  await ph.shutdown();
});

test("@railyard/analytics-node reads the env, defaults the host and sends through the same protocol", { skip: skipNoPg }, async () => {
  const { createServerAnalytics, readAnalyticsEnv, DEFAULT_HOST } = await import("../../../../packages/analytics-node/src/index.mjs");
  assert.deepEqual(readAnalyticsEnv({ NEXT_PUBLIC_ANALYTICS_SITE: site.token, PRODUCT_ANALYTICS_URL: run.base }), { site: site.token, host: run.base });
  assert.equal(createServerAnalytics({ env: { ANALYTICS_SITE: site.token } }).host, DEFAULT_HOST);
  assert.throws(() => createServerAnalytics({ env: {} }), /site key/);
  const a = createServerAnalytics({ env: { ANALYTICS_SITE: site.token, PRODUCT_ANALYTICS_URL: run.base } });
  a.capture("wrap-1", "wrapped_event", { ok: true }, { company: "acme" });
  a.identify("wrap-1", { plan: "team" });
  a.groupIdentify("company", "globex", { name: "Globex" });
  assert.equal(await a.isEnabled("new_checkout", "wrap-1"), true);
  assert.equal(await a.flagValue("new_checkout", "wrap-1"), "blue");
  await a.shutdown();
  await run.svc.deps.writer.flush();
  assert.equal(count(pg, `project_id = '${site.project_id}' and event = 'wrapped_event' and "$group_0" = 'acme'`), 1);
  assert.equal(pg.psql(`select p.properties ->> 'plan' from pa.persons p join pa.person_distinct_ids d on d.person_id = p.id where d.project_id = '${site.project_id}' and d.distinct_id = 'wrap-1'`), "team");
});
