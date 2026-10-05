// QY1.2 (A26): the analytics slot reaches product analytics. `sites` answers
// the analytics host and the project's public key next to the page-tag
// values; `product_analytics` answers the first product event and the web
// view, and the server key only when asked; the slot's proof falls back to
// the first product event; every call to the service is scoped to one app.
import { test } from "node:test";
import assert from "node:assert/strict";

import { MemoryRepo } from "../../../../../lib/repo.ts";
import { sign } from "../../../../../lib/standard-webhooks.ts";
import { MemorySites } from "../../../../../lib/sites.ts";
import type { InsightsClient } from "../../../../../lib/insights.ts";
import { productAnalyticsClient, type WebOverview } from "../../../../../lib/product-analytics.ts";
import { serverKeyFor } from "../../../../../../product-analytics/src/web/server-key.ts";
import { handleBridge, type BridgeRoute } from "./service.ts";
import { readFileSync } from "node:fs";

const contract = JSON.parse(readFileSync(new URL("../../../../../../../tests/contract/fixtures/scribase-bridge/contract.json", import.meta.url), "utf8"));

const SECRET = `whsec_${Buffer.from("scribase-bridge-test-secret-0032").toString("base64")}`;
const PA_SECRET = "pa-console-secret-".padEnd(40, "x");
const T0 = new Date("2026-10-05T12:00:00Z");
let n = 0;

const WEB: WebOverview = {
  range: { from: "2026-09-28T12:00:00.000Z", to: T0.toISOString() },
  totals: { visitors: 3, views: 7, sessions: 4, bounce_rate: 0.5 },
  series: [],
  pages: [{ value: "/pricing", views: 3, visitors: 3 }],
  referrers: [{ value: "$direct", views: 1, visitors: 1 }],
  utm: { source: [], medium: [], campaign: [] },
  vitals: {
    LCP: { p75: 1800, samples: 4, rating: "good" },
    INP: { p75: 120, samples: 4, rating: "good" },
    CLS: { p75: 0.05, samples: 4, rating: "good" },
    FCP: { p75: null, samples: 0, rating: null },
    TTFB: { p75: null, samples: 0, rating: null },
  },
};

/** A fake service: records each call's path and token claims; `first` is the first product event per app id. */
function fakePa(first: Map<string, { event: string; at: string }>) {
  const calls: { path: string; claims: { org_id: string; app_ids: string[]; role: string } }[] = [];
  const fetchImpl = (async (url: string, init: RequestInit) => {
    const u = new URL(url);
    const tok = String((init.headers as Record<string, string>).authorization).slice(7);
    const claims = JSON.parse(Buffer.from(tok.split(".")[1]!, "base64url").toString());
    calls.push({ path: u.pathname + u.search, claims });
    const app = u.pathname.split("/")[3]!;
    if (u.pathname.endsWith("/status")) return new Response(JSON.stringify({ first_event: first.has(app) ? { uuid: "x", ...first.get(app) } : null }), { status: 200 });
    if (u.pathname.endsWith("/web")) return new Response(JSON.stringify(WEB), { status: 200 });
    return new Response("{}", { status: 404 });
  }) as typeof fetch;
  return { calls, client: productAnalyticsClient({ url: "http://pa.internal:8790", secret: PA_SECRET }, "https://pa.example/", fetchImpl, () => T0) };
}

async function call(repo: MemoryRepo, route: BridgeRoute, payload: unknown, extra: Partial<Parameters<typeof handleBridge>[0]> = {}) {
  const body = JSON.stringify(payload);
  const id = `msg_${route}_${(++n).toString(16).padStart(24, "0")}`;
  const ts = String(Math.floor(T0.getTime() / 1000));
  return handleBridge({ repo, secret: SECRET, consoleUrl: "https://railyard.example", now: () => T0, ...extra }, route, { id, timestamp: ts, signature: await sign(SECRET, id, ts, body) }, body);
}

const addon = (slots = ["analytics", "deploy"]) => ({ active: true, slots, renews_at: "2026-11-05T12:00:00Z", caps: { apps: 5, analytics_events_month: 3_000_000 }, as_of: T0.getTime() });

test("sites answers the analytics host and the project key (the site key) next to the page-tag values; nothing of the Insights path is removed", async () => {
  const repo = new MemoryRepo(() => T0);
  const pa = fakePa(new Map());
  const extra = { sites: new MemorySites(), insightsUrl: "https://insights.example", productAnalytics: pa.client };
  await call(repo, "orgs", { scribase_org_id: "org_pa", name: "PA", addon: addon() }, extra);
  const r = await call(repo, "sites", { scribase_org_id: "org_pa", app: "shop-prod" }, extra);
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.equal(r.body.analytics_host, "https://pa.example");
  assert.equal(r.body.analytics_key, r.body.site_key);
  assert.equal(r.body.script_url, "https://insights.example/s.js");
  assert.equal(r.body.collector_url, "https://insights.example");
  // Without the service on this deployment the host is null; the rest is unchanged.
  const off = await call(repo, "sites", { scribase_org_id: "org_pa", app: "shop-prod" }, { sites: new MemorySites(), insightsUrl: "https://insights.example" });
  assert.equal(off.body.analytics_host, null);
  assert.equal(off.body.site_key, r.body.site_key);
});

test("product_analytics: first product event and web view for one app; the server key only when asked, and it is the service's own derivation", async () => {
  const repo = new MemoryRepo(() => T0);
  const first = new Map<string, { event: string; at: string }>();
  const pa = fakePa(first);
  const extra = { sites: new MemorySites(), productAnalytics: pa.client };
  const orgId = (await call(repo, "orgs", { scribase_org_id: "org_pa2", name: "PA2", addon: addon() }, extra)).body.org_id as string;
  const site = await call(repo, "sites", { scribase_org_id: "org_pa2", app: "shop-prod" }, extra);
  await call(repo, "sites", { scribase_org_id: "org_pa2", app: "shop-staging" }, extra);
  const appId = site.body.app_id as string;
  const before = await call(repo, "product_analytics", { scribase_org_id: "org_pa2", app: "shop-prod" }, extra);
  assert.equal(before.status, 200);
  assert.equal(before.body.available, true);
  assert.equal(before.body.first_event, null);
  assert.equal(before.body.host, "https://pa.example");
  assert.equal(before.body.key, site.body.site_key);
  assert.equal("server_key" in before.body, false, "no server key unless asked");
  assert.deepEqual(before.body.web, WEB);
  first.set(appId, { event: "scribase_proof", at: "2026-10-05T12:00:20.000Z" });
  const after = await call(repo, "product_analytics", { scribase_org_id: "org_pa2", app: "shop-prod", days: 30, with_server_key: true }, extra);
  assert.deepEqual(after.body.first_event, { event: "scribase_proof", at: "2026-10-05T12:00:20.000Z" });
  assert.equal(after.body.server_key, serverKeyFor(PA_SECRET, appId));
  // The published contract (v2, QY1.2 additions) names exactly these keys.
  assert.deepEqual(Object.keys(before.body).sort(), contract.product_analytics_answer_keys);
  assert.deepEqual(Object.keys(after.body).sort(), [...contract.product_analytics_answer_keys, contract.product_analytics_server_key.answer_field].sort());
  assert.match(String(after.body.server_key), new RegExp(contract.product_analytics_server_key.format));
  assert.deepEqual(Object.keys(after.body.web as object), contract.web_keys);
  // Every call to the service was scoped to this one app of this org, read only.
  for (const c of pa.calls) {
    assert.deepEqual(c.claims.app_ids, [appId]);
    assert.equal(c.claims.org_id, orgId);
    assert.equal(c.claims.role, "viewer");
  }
  assert.ok(pa.calls.some((c) => c.path === `/v1/projects/${appId}/web?days=30`));
  // Input checks and the add-on gate.
  assert.equal((await call(repo, "product_analytics", { scribase_org_id: "org_pa2", app: "shop-prod", days: 5 }, extra)).status, 400);
  assert.equal((await call(repo, "product_analytics", { scribase_org_id: "org_pa2", app: "nope" }, extra)).body.available, false);
  assert.equal((await call(repo, "product_analytics", { scribase_org_id: "org_pa2", app: "shop-prod" }, { sites: new MemorySites() })).body.available, false);
  await call(repo, "orgs", { scribase_org_id: "org_noan", name: "N", addon: addon(["deploy"]) }, extra);
  assert.equal((await call(repo, "product_analytics", { scribase_org_id: "org_noan", app: "shop-prod" }, extra)).status, 402);
});

test("the analytics proof is the first product event when no page-tag event has arrived; the tab carries both times", async () => {
  const repo = new MemoryRepo(() => T0);
  const first = new Map<string, { event: string; at: string }>();
  const pa = fakePa(first);
  const insights: InsightsClient = {
    summary: async (orgId: string) => ({
      org_id: orgId,
      since: T0.toISOString(),
      apps: (await repo.listApps(orgId)).map((a) => ({ app_id: a.id, first_event_at: null, events: 0, visitors: 0 })),
      funnel: { steps: [{ name: "visit" as const, sessions: 0 }, { name: "signup" as const, sessions: 0 }, { name: "first_action" as const, sessions: 0 }] },
    }),
  };
  const extra = { sites: new MemorySites(), productAnalytics: pa.client, insights };
  await call(repo, "orgs", { scribase_org_id: "org_pf", name: "PF", addon: addon() }, extra);
  const appId = (await call(repo, "sites", { scribase_org_id: "org_pf", app: "shop-prod" }, extra)).body.app_id as string;
  assert.equal((await call(repo, "proof", { scribase_org_id: "org_pf", slot: "analytics" }, extra)).body.state, "no_proof_yet");
  first.set(appId, { event: "$pageview", at: "2026-10-05T12:00:40.000Z" });
  const proof = await call(repo, "proof", { scribase_org_id: "org_pf", slot: "analytics" }, extra);
  assert.deepEqual(proof.body.proof, { kind: "first_product_event", id: appId, at: "2026-10-05T12:00:40.000Z" });
  const tab = await call(repo, "insights", { scribase_org_id: "org_pf", app: "shop-prod" }, extra);
  assert.equal(tab.body.first_event_at, null);
  assert.equal(tab.body.first_product_event_at, "2026-10-05T12:00:40.000Z");
  // A service error reads as "not yet", never as a failure of the tab.
  const broken = productAnalyticsClient({ url: "http://pa.internal", secret: PA_SECRET }, "https://pa.example", (async () => new Response("{}", { status: 500 })) as unknown as typeof fetch, () => T0);
  const tab2 = await call(repo, "insights", { scribase_org_id: "org_pf", app: "shop-prod" }, { ...extra, productAnalytics: broken });
  assert.equal(tab2.status, 200);
  assert.equal(tab2.body.first_product_event_at, null);
});
