// Site entries for the edge collector (RYP-E1) and the Insights tenant client (PY1.3).
import { test } from "node:test";
import assert from "node:assert/strict";

import { addApp } from "../lib/apps.ts";
import { firstEvent, insightsClient } from "../lib/insights.ts";
import { applyLadder } from "../lib/ladder.ts";
import { MemoryRepo } from "../lib/repo.ts";
import { MemorySites, registerSite, syncSites } from "../lib/sites.ts";
import { emptyEntitlement } from "../lib/types.ts";

const now = new Date("2026-09-29T12:00:00Z");
const ORG = "0b8f6a3e-1111-4a3e-8b8f-0b8f6a3e1111";

test("an app's site entry carries the org, its cap and the pause; a pause rewrites every entry", async () => {
  const repo = new MemoryRepo(() => now);
  const org = await repo.createOrgForUser({ userId: "u", email: "a@x.com", name: "a" });
  await repo.saveEntitlement(applyLadder(emptyEntitlement(org.id, now), { kind: "card_check_paid", paymentId: "p", customerId: null, at: now }).ent);
  const sites = new MemorySites();
  const app = await addApp(repo, org.id, "shop", now);
  await registerSite(sites, repo, app, now);
  assert.deepEqual(sites.entries.get(`site:${app.site_key}`), { org_id: org.id, app_id: app.id, events_limit: 100_000, paused: false });
  const ent = await repo.getEntitlement(org.id);
  await repo.saveEntitlement({ ...ent!, paused: true, status: "paused" });
  assert.equal(await syncSites(sites, repo, org.id, now), 1);
  assert.equal(sites.entries.get(`site:${app.site_key}`)?.paused, true);
});

test("the Insights client sends the bearer token only as a header and validates the answer", async () => {
  const calls: { url: string; auth: string | null }[] = [];
  const body = {
    org_id: ORG,
    since: "2026-09-22T12:00:00.000Z",
    apps: [
      { app_id: "5d1c0000-aaaa-4aaa-8aaa-5d1c0000aaaa", first_event_at: "2026-09-29T11:00:00.000Z", events: 4, visitors: 2 },
      { app_id: "5d1c0000-bbbb-4bbb-8bbb-5d1c0000bbbb", first_event_at: null, events: 0, visitors: 0 },
    ],
    funnel: { steps: [{ name: "visit", sessions: 2 }, { name: "signup", sessions: 1 }, { name: "first_action", sessions: 1 }] },
  };
  const client = insightsClient({
    apiUrl: "https://insights.internal/",
    token: "tok_0123456789abcdef",
    fetchImpl: (async (url: string, init: RequestInit) => {
      calls.push({ url, auth: new Headers(init.headers).get("authorization") });
      return new Response(JSON.stringify(body), { status: 200 });
    }) as typeof fetch,
  });
  const s = await client.summary(ORG, { days: 7 });
  assert.equal(calls[0].url, `https://insights.internal/api/v1/tenants/${ORG}/summary?days=7`);
  assert.equal(calls[0].auth, "Bearer tok_0123456789abcdef");
  assert.ok(!calls[0].url.includes("tok_"));
  assert.deepEqual(firstEvent(s), { app_id: body.apps[0].app_id, at: "2026-09-29T11:00:00.000Z" });
  assert.equal(firstEvent(s, body.apps[1].app_id), null);
  await assert.rejects(client.summary("nope"), /invalid/);
  const bad = insightsClient({ apiUrl: "https://i", token: "t", fetchImpl: (async () => new Response("{}", { status: 200 })) as typeof fetch });
  await assert.rejects(bad.summary(ORG), /unexpected shape/);
});

test("the Insights page model: rows per app and the funnel with step-to-step rates", async () => {
  const { insightsView } = await import("../lib/insights-view.ts");
  const v = insightsView(
    {
      org_id: ORG,
      since: "x",
      apps: [{ app_id: "5d1c0000-aaaa-4aaa-8aaa-5d1c0000aaaa", first_event_at: "2026-09-29T11:00:00.000Z", events: 4, visitors: 2 }],
      funnel: { steps: [{ name: "visit", sessions: 4 }, { name: "signup", sessions: 2 }, { name: "first_action", sessions: 1 }] },
    },
    new Map([["5d1c0000-aaaa-4aaa-8aaa-5d1c0000aaaa", "shop"]]),
  );
  assert.deepEqual(v.apps, [{ name: "shop", firstEvent: "2026-09-29 11:00 UTC", visitors: 2, events: 4 }]);
  assert.deepEqual(v.steps.map((s) => [s.label, s.sessions, s.ofPrevious]), [["Visited", 4, null], ["Signed up", 2, "50%"], ["Did a first action", 1, "50%"]]);
  assert.equal(v.empty, false);
});

test("the console's caps module is a byte-for-byte copy of the edge collector's (one message everywhere)", async () => {
  const { readFileSync } = await import("node:fs");
  const { join, dirname } = await import("node:path");
  const { fileURLToPath } = await import("node:url");
  const here = dirname(fileURLToPath(import.meta.url));
  assert.equal(readFileSync(join(here, "../lib/caps.ts"), "utf8"), readFileSync(join(here, "../../insights-edge/src/caps.ts"), "utf8"));
  const { insightsView } = await import("../lib/insights-view.ts");
  const v = insightsView(
    { org_id: ORG, since: "x", apps: [{ app_id: "a", first_event_at: null, events: 5, visitors: 1, month_events: 100 }], funnel: { steps: [{ name: "visit", sessions: 0 }, { name: "signup", sessions: 0 }, { name: "first_action", sessions: 0 }] } },
    new Map(),
    100,
    new Date("2026-09-29T12:00:00Z"),
  );
  assert.match(v.capNote ?? "", /still counted, and are not stored until 2026-10-01/);
});
