// QY1.5 (I6): the console side of custom domains. The Cloudflare client is a
// byte copy of the CLI's; attach and detach use the org's sealed grant; the
// record path takes the CLI's report; one org never touches another's
// hostname; the Scribase bridge lists the domains with their state.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import { MemoryRepo } from "../repo.ts";
import { addApp } from "../apps.ts";
import { applyLadder } from "../ladder.ts";
import { emptyEntitlement } from "../types.ts";
import { seal } from "../ai/seal.ts";
import { sign } from "../standard-webhooks.ts";
import { CloudflareOAuth } from "./cloudflare.ts";
import { MemoryShipStore } from "./store.ts";
import { addAppDomain, appDomains, productionWorker, recordAppDomain, removeAppDomain } from "./domains.ts";
import { ShipError, type ShipDeps } from "./service.ts";
import { handleBridge } from "../../app/api/v1/bridge/scribase/service.ts";
import { startCloudflare } from "../../../../packages/cli/test/fixtures/fake-cloudflare.mjs";
import { workerName } from "../../../../packages/cli/src/detect.mjs";

const KEY = Buffer.alloc(32, 9).toString("base64");
const NOW = new Date("2026-10-05T12:00:00Z");

test("the Cloudflare domains client is a byte copy of the CLI's", () => {
  for (const [mine, theirs] of [["cf-domains.mjs", "domains.mjs"], ["cf-domains.d.mts", "domains.d.mts"]]) {
    assert.equal(readFileSync(new URL(`./${mine}`, import.meta.url), "utf8"), readFileSync(new URL(`../../../../packages/cli/src/${theirs}`, import.meta.url), "utf8"), mine);
  }
});

test("the production Worker is named as the CLI names it", () => {
  for (const [app, branch] of [["shop", "main"], ["Shop App", "main"], ["shop", "release"], ["a".repeat(50), "prod/x"]]) assert.equal(productionWorker(app, branch), workerName(app, branch));
});

async function setup() {
  const cf = await startCloudflare({ zones: ["example.com"] });
  const repo = new MemoryRepo(() => NOW);
  const a = await repo.createOrgForUser({ userId: "ua", email: "a@x.example", name: "A" });
  const b = await repo.createOrgForUser({ userId: "ub", email: "b@x.example", name: "B" });
  for (const o of [a, b]) await repo.saveEntitlement(applyLadder(emptyEntitlement(o.id, NOW), { kind: "card_check_paid", paymentId: `p_${o.id}`, customerId: null, at: NOW }).ent);
  const store = new MemoryShipStore();
  const deps = {
    store,
    repo,
    cloudflare: new CloudflareOAuth({ clientId: "c", clientSecret: "s", redirectUri: "http://x", scopes: [], tokenUrl: `${cf.base}/oauth2/token`, apiBase: cf.api }),
    github: null,
    oidcKeys: async () => [],
    consoleUrl: "http://console",
    credentialsKey: KEY,
    sessionSecret: "s".repeat(32),
  } as unknown as ShipDeps;
  const grant = async (orgId: string) => {
    const r = await seal(KEY, `railyard:cloudflare:refresh:${orgId}`, "cf-refresh-1");
    await store.saveCloudflare({ org_id: orgId, account_id: cf.account, account_name: "A", refresh_nonce: r.nonce, refresh_ciphertext: r.ciphertext, access_nonce: null, access_ciphertext: null, access_expires_at: null, updated_at: NOW.toISOString() });
  };
  return { cf, repo, a, b, store, deps, grant };
}

test("attach and detach with the org's grant; refusals for another org's hostname and for no grant", async () => {
  const { cf, repo, a, b, deps, grant } = await setup();
  try {
    await addApp(repo, a.id, "shop", NOW);
    await addApp(repo, b.id, "blog", NOW);
    await assert.rejects(addAppDomain(deps, a.id, { app: "shop", hostname: "app.example.com" }, NOW), (e: ShipError & { code?: string }) => e.status === 409 && e.code === "no_cloudflare_grant");
    await grant(a.id);
    const d = await addAppDomain(deps, a.id, { app: "shop", hostname: "App.Example.com" }, NOW);
    assert.deepEqual([d.hostname, d.worker, d.state, d.zone_name], ["app.example.com", "shop", "pending", "example.com"]);
    assert.ok(!JSON.stringify(await deps.store.listDomains(a.id)).includes("cf-access"), "no token in the record");
    await assert.rejects(addAppDomain(deps, a.id, { app: "nope", hostname: "x.example.com" }, NOW), (e: ShipError) => e.status === 404);
    await assert.rejects(addAppDomain(deps, a.id, { app: "shop", hostname: "x.workers.dev" }, NOW), (e: ShipError) => e.status === 400);
    // Org B cannot remove org A's hostname: it has no record, so nothing is detached on any account.
    await grant(b.id);
    assert.deepEqual(await removeAppDomain(deps, b.id, { app: "blog", hostname: "app.example.com" }, NOW), { hostname: "app.example.com", removed: false });
    assert.equal(cf.domains.size, 1, "A's domain is still attached");
    assert.deepEqual(await removeAppDomain(deps, a.id, { app: "shop", hostname: "app.example.com" }, NOW), { hostname: "app.example.com", removed: true });
    assert.deepEqual(await removeAppDomain(deps, a.id, { app: "shop", hostname: "app.example.com" }, NOW), { hostname: "app.example.com", removed: false }, "idempotent");
    assert.equal(cf.domains.size, 0);
  } finally {
    await cf.close();
  }
});

test("the CLI's report is recorded; states refresh from Cloudflare with a grant; the bridge lists them for the Deploy tab", async () => {
  const { cf, repo, a, deps, grant } = await setup();
  try {
    await addApp(repo, a.id, "shop", NOW);
    assert.deepEqual(await recordAppDomain(deps, a.id, { app: "shop", hostname: "live.example.com", state: "pending", cf_domain_id: "dom9", zone_name: "example.com" }, NOW), { recorded: true });
    assert.equal((await appDomains(deps, a.id, "shop", NOW))[0].state, "pending", "without a grant: as recorded");
    await grant(a.id);
    cf.domains.set("dom9", { id: "dom9", hostname: "live.example.com", service: "shop", zone_id: `zone0${"0".repeat(27)}`, zone_name: "example.com" });
    assert.equal((await appDomains(deps, a.id, "shop", NOW))[0].state, "active", "refreshed: the certificate is issued");
    // The Scribase bridge (signed) answers the org's domains.
    const SECRET = `whsec_${Buffer.from("scribase-bridge-test-secret-0032").toString("base64")}`;
    await repo.bindScribaseOrg("org_dom", a.id);
    const ent = (await repo.getEntitlement(a.id))!;
    await repo.saveAddonEntitlement({ ...ent, plan: "addon", status: "active", addon_slots: ["deploy"], apps_limit: 5 }, NOW.getTime());
    const body = JSON.stringify({ scribase_org_id: "org_dom" });
    const id = `msg_domains_${"1".repeat(24)}`;
    const ts = String(Math.floor(NOW.getTime() / 1000));
    const r = await handleBridge({ repo, secret: SECRET, consoleUrl: "http://console", now: () => NOW, ship: deps }, "domains", { id, timestamp: ts, signature: await sign(SECRET, id, ts, body) }, body);
    assert.equal(r.status, 200, JSON.stringify(r.body));
    assert.deepEqual((r.body.domains as { hostname: string; state: string }[]).map((d) => [d.hostname, d.state]), [["live.example.com", "active"]]);
    assert.deepEqual(await recordAppDomain(deps, a.id, { app: "shop", hostname: "live.example.com", removed: true }, NOW), { recorded: true });
  } finally {
    await cf.close();
  }
});
