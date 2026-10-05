// QY1.2 (A26) end to end on a real service and Postgres: the console bridge
// (its own code, an in-memory repo) creates the Scribase project's app; one
// event sent the way an app's SDK sends it, with the key `scribase add
// analytics` writes, reaches the slot proof as `first_product_event`, within
// 60 s (target; measured here), and the tab's web view reads the same store.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { flushed, makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { MemoryRepo } from "../../../console/lib/repo.ts";
import { MemorySites } from "../../../console/lib/sites.ts";
import { sign } from "../../../console/lib/standard-webhooks.ts";
import { productAnalyticsClient } from "../../../console/lib/product-analytics.ts";
import { handleBridge, type BridgeRoute } from "../../../console/app/api/v1/bridge/scribase/service.ts";

const BRIDGE = `whsec_${Buffer.from("scribase-bridge-test-secret-0032").toString("base64")}`;
const PA_SECRET = "qy12-console-secret-".padEnd(40, "z");
let pg: ScratchPg;
let run: Running;
let n = 0;

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [], { consoleSecret: PA_SECRET });
});

test("first product event: bridge app -> SDK event with the written key -> slot proof and web view", { skip: skipNoPg }, async () => {
  const repo = new MemoryRepo(() => new Date());
  const pa = productAnalyticsClient({ url: run.base, secret: PA_SECRET }, "https://pa.example");
  const deps = { repo, secret: BRIDGE, consoleUrl: "https://railyard.example", now: () => new Date(), sites: new MemorySites(), insightsUrl: "https://insights.example", productAnalytics: pa };
  const call = async (route: BridgeRoute, payload: unknown) => {
    const body = JSON.stringify(payload);
    const id = `msg_${route}_${(++n).toString(16).padStart(24, "0")}`;
    const ts = String(Math.floor(Date.now() / 1000));
    return handleBridge(deps, route, { id, timestamp: ts, signature: await sign(BRIDGE, id, ts, body) }, body);
  };
  const addon = { active: true, slots: ["analytics"], renews_at: null, caps: { apps: 5, analytics_events_month: 100_000 }, as_of: Date.now() };
  const orgId = (await call("orgs", { scribase_org_id: "org_e2e", name: "E2E", addon })).body.org_id as string;
  const site = await call("sites", { scribase_org_id: "org_e2e", app: "shop-prod" });
  assert.equal(site.status, 200);
  // The service's site directory sees the console's app (as the PostgREST directory reads railyard.apps).
  run.sites.sites.push(makeSite({ project_id: site.body.app_id as string, org_id: orgId, token: site.body.analytics_key as string, name: "shop-prod", scribase_org_id: "org_e2e" }));

  const started = Date.now();
  const sent = await fetch(`${run.base}/capture/`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ api_key: site.body.analytics_key, event: "scribase_proof", distinct_id: "scribase-cli", properties: { $current_url: "https://shop.example/", $pathname: "/" } }),
  });
  assert.equal(sent.status, 200);
  let proof: Record<string, unknown> | null = null;
  while (Date.now() - started < 60_000) {
    await flushed(run);
    const r = await call("proof", { scribase_org_id: "org_e2e", slot: "analytics" });
    if (r.body.state === "live") {
      proof = r.body.proof as Record<string, unknown>;
      break;
    }
    await new Promise((res) => setTimeout(res, 250));
  }
  const took = Date.now() - started;
  assert.ok(proof, "the first product event shows within 60 s");
  assert.equal(proof!.kind, "first_product_event");
  assert.equal(proof!.id, site.body.app_id);
  console.log(`first product event visible after ${took} ms`);

  const tab = await call("product_analytics", { scribase_org_id: "org_e2e", app: "shop-prod", days: 1, with_server_key: true });
  assert.equal(tab.body.available, true, JSON.stringify(tab.body));
  assert.equal((tab.body.first_event as { event: string }).event, "scribase_proof");
  // The server key the bridge hands Scribase is accepted by the service's server door.
  const fwd = await fetch(`${run.base}/v1/server/${site.body.app_id}/batch`, {
    method: "POST",
    headers: { "content-type": "application/json", authorization: `Bearer ${tab.body.server_key}` },
    body: JSON.stringify({ batch: [{ event: "$pageview", distinct_id: "user_1", properties: { $current_url: "https://shop.example/pricing", $pathname: "/pricing" } }] }),
  });
  assert.equal(fwd.status, 200, await fwd.text());
  await flushed(run);
  const web = (await call("product_analytics", { scribase_org_id: "org_e2e", app: "shop-prod", days: 1 })).body.web as { totals: { views: number }; pages: { value: string }[] };
  assert.equal(web.totals.views, 1);
  assert.deepEqual(web.pages.map((p) => p.value), ["/pricing"]);
});
