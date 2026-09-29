// The Scribase bridge (Wave P PY1.0, RYP-E6): Scribase's control-api creates the
// Railyard org on first use, bound to the Scribase org id, with no second
// sign-up; keys reach only the Scribase CLI's secure channel; each slot answers
// {slot, live, proof:{kind,id,at}} or "arriving".
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { MemoryRepo } from "../../../../../lib/repo.ts";
import { sign } from "../../../../../lib/standard-webhooks.ts";
import { pollDevice } from "../../../../../lib/tokens.ts";
import { addApp } from "../../../../../lib/apps.ts";
import { MemorySites } from "../../../../../lib/sites.ts";
import type { InsightsClient } from "../../../../../lib/insights.ts";
import { access } from "../../../../../lib/ladder.ts";
import { recordDeploy, parseDeployInput } from "../../deploys/service.ts";
import {
  BRIDGE_SLOTS,
  CLAIM_TTL_SECONDS,
  handleBridge,
  ownerIdFor,
  slotProof,
} from "./service.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const SECRET = `whsec_${Buffer.from("scribase-bridge-test-secret-0032").toString("base64")}`;
const T0 = new Date("2026-09-29T12:00:00Z");

let n = 0;
async function call(
  repo: MemoryRepo,
  route: "orgs" | "claims" | "proof" | "deploys" | "sites" | "insights" | "ai" | "ai_usage",
  payload: unknown,
  opts: { at?: Date; secret?: string; id?: string; extra?: Partial<Parameters<typeof handleBridge>[0]> } = {},
) {
  const at = opts.at ?? T0;
  const body = JSON.stringify(payload);
  const id = opts.id ?? `msg_${++n}`;
  const ts = String(Math.floor(at.getTime() / 1000));
  const signature = await sign(opts.secret ?? SECRET, id, ts, body);
  return handleBridge(
    { repo, secret: SECRET, consoleUrl: "https://railyard.ereboslab.com", now: () => at, ...opts.extra },
    route,
    { id, timestamp: ts, signature },
    body,
  );
}

const addon = (active = true, slots = ["deploy", "analytics", "ai"]) => ({
  active,
  slots,
  renews_at: "2026-10-29T12:00:00Z",
  caps: { apps: 5, analytics_events_month: 3_000_000 },
});

test("first use creates one Railyard org bound to the Scribase org; a second call returns the same org", async () => {
  const repo = new MemoryRepo(() => T0);
  const r1 = await call(repo, "orgs", { scribase_org_id: "org_abc", name: "Acme", addon: addon() });
  assert.equal(r1.status, 200);
  assert.equal(r1.body.created, true);
  const orgId = r1.body.org_id as string;
  const r2 = await call(repo, "orgs", { scribase_org_id: "org_abc", name: "Acme renamed", addon: addon() });
  assert.equal(r2.body.created, false);
  assert.equal(r2.body.org_id, orgId);
  assert.equal(repo.orgs.size, 1);
  const org = await repo.getOrg(orgId);
  assert.equal(org?.owner_user_id, await ownerIdFor("org_abc"));
  const ent = await repo.getEntitlement(orgId);
  assert.equal(ent?.plan, "addon");
  assert.deepEqual(ent?.addon_slots, ["ai", "analytics", "deploy"]);
  assert.equal(ent?.apps_limit, 5);
  assert.equal(ent?.events_limit, 3_000_000);
  assert.equal(access(ent, T0).canAddApps, true);
  // No second sign-up: nothing in the org list asks for an email or a password.
  assert.equal(repo.members.length, 1);
  assert.match(repo.members[0].email, /@scribase\.invalid$/);
});

test("a lapsed add-on pauses the org and keeps its apps and deploys", async () => {
  const repo = new MemoryRepo(() => T0);
  const { body } = await call(repo, "orgs", { scribase_org_id: "org_keep", name: "Keep", addon: addon() });
  const orgId = body.org_id as string;
  await addApp(repo, orgId, "shop", T0);
  const r = await call(repo, "orgs", { scribase_org_id: "org_keep", name: "Keep", addon: addon(false) });
  assert.equal(r.status, 200);
  const a = access(await repo.getEntitlement(orgId), T0);
  assert.equal(a.state, "paused");
  assert.equal(a.via, "addon");
  assert.equal((await repo.listApps(orgId)).length, 1, "apps are kept");
  const d = await recordDeploy(repo, orgId, parseDeployInput({ app: "shop", branch: "main", framework: "vite", status: "succeeded" }) as never, T0);
  assert.equal(d.status, 402);
  assert.match(String(d.body.error), /renew the add-on in Scribase billing/);
});

test("requests must carry a valid Standard Webhooks signature, a fresh timestamp and a new id", async () => {
  const repo = new MemoryRepo(() => T0);
  const payload = { scribase_org_id: "org_sig", name: "Sig", addon: addon() };
  const bad = await call(repo, "orgs", payload, { secret: `whsec_${Buffer.from("another-secret-another-secret-32").toString("base64")}` });
  assert.equal(bad.status, 401);
  const stale = await handleBridge(
    { repo, secret: SECRET, consoleUrl: "https://c", now: () => new Date(T0.getTime() + 600_000) },
    "orgs",
    { id: "msg_stale", timestamp: String(Math.floor(T0.getTime() / 1000)), signature: await sign(SECRET, "msg_stale", String(Math.floor(T0.getTime() / 1000)), JSON.stringify(payload)) },
    JSON.stringify(payload),
  );
  assert.equal(stale.status, 401);
  const ok = await call(repo, "orgs", payload, { id: "msg_once" });
  assert.equal(ok.status, 200);
  const replay = await call(repo, "orgs", payload, { id: "msg_once" });
  assert.equal(replay.status, 409);
  assert.equal(repo.orgs.size, 1);
});

test("input is validated at the boundary", async () => {
  const repo = new MemoryRepo(() => T0);
  for (const bad of [
    {},
    { scribase_org_id: "", name: "x", addon: addon() },
    { scribase_org_id: "org x;drop", name: "x", addon: addon() },
    { scribase_org_id: "org_ok", name: "", addon: addon() },
    { scribase_org_id: "org_ok", name: "x", addon: { ...addon(), slots: ["deploy", "shell"] } },
    { scribase_org_id: "org_ok", name: "x", addon: { ...addon(), caps: { apps: -1, analytics_events_month: 1 } } },
    { scribase_org_id: "org_ok", name: "x", addon: { ...addon(), active: "yes" } },
  ]) {
    const r = await call(repo, "orgs", bad);
    assert.equal(r.status, 400, JSON.stringify(bad));
  }
  const notJson = await handleBridge(
    { repo, secret: SECRET, consoleUrl: "https://c", now: () => T0 },
    "orgs",
    { id: "m-nj", timestamp: String(Math.floor(T0.getTime() / 1000)), signature: await sign(SECRET, "m-nj", String(Math.floor(T0.getTime() / 1000)), "{nope") },
    "{nope",
  );
  assert.equal(notJson.status, 400);
});

test("a claim is a one-time, pre-approved device code: the Scribase CLI exchanges it for a token once", async () => {
  const repo = new MemoryRepo(() => T0);
  const { body } = await call(repo, "orgs", { scribase_org_id: "org_claim", name: "Claim", addon: addon() });
  const orgId = body.org_id as string;
  const c = await call(repo, "claims", { scribase_org_id: "org_claim", slot: "deploy" });
  assert.equal(c.status, 200);
  assert.equal(c.body.expires_in, CLAIM_TTL_SECONDS);
  assert.equal(c.body.token_endpoint, "https://railyard.ereboslab.com/api/v1/device/token");
  // The claim is not a token and holds no org secret.
  assert.doesNotMatch(JSON.stringify(c.body), /ry_[A-Za-z0-9_-]{20,}/);
  const first = await pollDevice(repo, c.body.claim as string, T0);
  assert.equal(first.status, 200);
  assert.equal((first.body as { org_id: string }).org_id, orgId);
  const again = await pollDevice(repo, c.body.claim as string, T0);
  assert.equal(again.status, 400);
  const late = await call(repo, "claims", { scribase_org_id: "org_claim", slot: "deploy" });
  const expired = await pollDevice(repo, late.body.claim as string, new Date(T0.getTime() + (CLAIM_TTL_SECONDS + 1) * 1000));
  assert.equal(expired.status, 400);
});

test("claims are refused for an unknown org, an inactive add-on or a slot the add-on does not include", async () => {
  const repo = new MemoryRepo(() => T0);
  assert.equal((await call(repo, "claims", { scribase_org_id: "org_none", slot: "deploy" })).status, 404);
  await call(repo, "orgs", { scribase_org_id: "org_off", name: "Off", addon: addon(false) });
  const off = await call(repo, "claims", { scribase_org_id: "org_off", slot: "deploy" });
  assert.equal(off.status, 402);
  assert.match(String(off.body.error), /add-on/);
  await call(repo, "orgs", { scribase_org_id: "org_some", name: "Some", addon: addon(true, ["analytics"]) });
  assert.equal((await call(repo, "claims", { scribase_org_id: "org_some", slot: "deploy" })).status, 402);
  assert.equal((await call(repo, "claims", { scribase_org_id: "org_some", slot: "nope" })).status, 400);
});

test("org A's claim never yields a token for org B", async () => {
  const repo = new MemoryRepo(() => T0);
  const a = (await call(repo, "orgs", { scribase_org_id: "org_a", name: "A", addon: addon() })).body.org_id;
  const b = (await call(repo, "orgs", { scribase_org_id: "org_b", name: "B", addon: addon() })).body.org_id;
  assert.notEqual(a, b);
  const c = await call(repo, "claims", { scribase_org_id: "org_b", slot: "analytics" });
  const t = await pollDevice(repo, c.body.claim as string, T0);
  assert.equal((t.body as { org_id: string }).org_id, b);
});

test("proof: every slot answers {slot, live, proof}; slots whose package has not landed say arriving", async () => {
  const repo = new MemoryRepo(() => T0);
  const orgId = (await call(repo, "orgs", { scribase_org_id: "org_p", name: "P", addon: addon(true, ["deploy", "analytics", "flags"]) })).body.org_id as string;
  for (const slot of Object.keys(BRIDGE_SLOTS)) {
    const r = await call(repo, "proof", { scribase_org_id: "org_p", slot });
    assert.equal(r.status, 200, slot);
    assert.deepEqual(Object.keys(r.body).sort(), ["live", "next", "proof", "slot", "state"], slot);
    assert.equal(r.body.slot, slot);
    if (!BRIDGE_SLOTS[slot as keyof typeof BRIDGE_SLOTS].landed) {
      assert.equal(r.body.state, "arriving", slot);
      assert.equal(r.body.live, false, slot);
      assert.equal(r.body.proof, null, slot);
    }
  }
  // A real deploy is the deploy proof, once the deploy slot is live.
  const pd = await slotProof(repo, orgId, "deploy", { deploy: true });
  assert.equal(pd.proof, null);
  await addApp(repo, orgId, "shop", T0);
  const d = await recordDeploy(repo, orgId, parseDeployInput({ app: "shop", branch: "main", framework: "vite", status: "succeeded", url: "https://shop.acct.workers.dev" }) as never, T0);
  const p = await slotProof(repo, orgId, "deploy", { deploy: true });
  assert.equal(p.live, true);
  assert.deepEqual(p.proof, { kind: "deploy", id: (d.body.deploy as { id: string }).id, at: T0.toISOString() });
});

test("no bridge handler writes a key or token to stdout, stderr or a log (grep test)", () => {
  const files: string[] = [];
  const walk = (dir: string) => {
    for (const f of readdirSync(dir)) {
      const p = join(dir, f);
      if (statSync(p).isDirectory()) walk(p);
      else if (/\.ts$/.test(f) && !f.endsWith(".test.ts")) files.push(p);
    }
  };
  walk(HERE);
  assert.ok(files.length >= 5, "service and four routes");
  for (const f of files) {
    const src = readFileSync(f, "utf8");
    assert.doesNotMatch(src, /console\.(log|info|warn|error|debug)|process\.(stdout|stderr)/, f);
  }
});

test("deploy slot (PY1.2): proof is the latest successful deploy, and the Scribase tab lists only this org's deploys", async () => {
  const repo = new MemoryRepo(() => T0);
  const a = (await call(repo, "orgs", { scribase_org_id: "org_da", name: "A", addon: addon() })).body.org_id as string;
  const b = (await call(repo, "orgs", { scribase_org_id: "org_db", name: "B", addon: addon() })).body.org_id as string;
  const none = await call(repo, "proof", { scribase_org_id: "org_da", slot: "deploy" });
  assert.equal(none.body.state, "no_proof_yet");
  assert.equal(none.body.live, true);
  await addApp(repo, a, "shop", T0);
  await addApp(repo, b, "shop", T0);
  const d = await recordDeploy(repo, a, parseDeployInput({ app: "shop", branch: "feat/pay", framework: "vite", status: "succeeded", url: "https://shop-feat-pay.acct.workers.dev" }) as never, T0);
  await recordDeploy(repo, b, parseDeployInput({ app: "shop", branch: "main", framework: "vite", status: "succeeded", url: "https://shop.other.workers.dev" }) as never, T0);
  const proof = await call(repo, "proof", { scribase_org_id: "org_da", slot: "deploy" });
  assert.equal(proof.body.state, "live");
  assert.deepEqual(proof.body.proof, { kind: "deploy", id: (d.body.deploy as { id: string }).id, at: T0.toISOString() });
  const list = await call(repo, "deploys", { scribase_org_id: "org_da" });
  assert.equal(list.status, 200);
  const deploys = list.body.deploys as { url: string; branch: string }[];
  assert.deepEqual(deploys.map((x) => x.url), ["https://shop-feat-pay.acct.workers.dev"]);
  assert.equal(((await call(repo, "deploys", { scribase_org_id: "org_da", branch: "main" })).body.deploys as unknown[]).length, 0);
  assert.deepEqual((await call(repo, "deploys", { scribase_org_id: "org_unknown" })).body, { deploys: [] });
});

test("analytics slot (PY1.3): sites gives the project's app its public page-tag settings and writes the collector entry; the proof is the first event", async () => {
  const repo = new MemoryRepo(() => T0);
  const sites = new MemorySites();
  let first: string | null = null;
  const insights: InsightsClient = {
    summary: async (orgId) => ({
      org_id: orgId,
      since: T0.toISOString(),
      apps: (await repo.listApps(orgId)).map((a) => ({ app_id: a.id, first_event_at: first, events: first ? 1 : 0, visitors: first ? 1 : 0 })),
      funnel: { steps: [{ name: "visit", sessions: 0 }, { name: "signup", sessions: 0 }, { name: "first_action", sessions: 0 }] },
    }),
  };
  const extra = { sites, insights, insightsUrl: "https://insights.example/" };
  const orgId = (await call(repo, "orgs", { scribase_org_id: "org_an", name: "An", addon: addon() }, { extra })).body.org_id as string;
  const r = await call(repo, "sites", { scribase_org_id: "org_an", app: "Shop Prod" }, { extra });
  assert.equal(r.status, 200, JSON.stringify(r.body));
  assert.equal(r.body.app, "shop-prod");
  assert.match(String(r.body.site_key), /^ry_site_/);
  assert.equal(r.body.script_url, "https://insights.example/s.js");
  assert.deepEqual(sites.entries.get(`site:${r.body.site_key}`), { org_id: orgId, app_id: r.body.app_id, events_limit: 3_000_000, paused: false });
  const again = await call(repo, "sites", { scribase_org_id: "org_an", app: "shop-prod" }, { extra });
  assert.equal(again.body.app_id, r.body.app_id, "the same project keeps its app");
  const before = await call(repo, "proof", { scribase_org_id: "org_an", slot: "analytics" }, { extra });
  assert.equal(before.body.state, "no_proof_yet");
  first = "2026-09-29T12:00:30.000Z";
  const after = await call(repo, "proof", { scribase_org_id: "org_an", slot: "analytics" }, { extra });
  assert.deepEqual(after.body.proof, { kind: "first_event", id: r.body.app_id, at: first });
  const tab = await call(repo, "insights", { scribase_org_id: "org_an", app: "shop-prod" }, { extra });
  assert.equal(tab.body.available, true);
  assert.equal(tab.body.first_event_at, first);
  assert.equal(tab.body.events_limit, 3_000_000);
  assert.equal(tab.body.cap_note, null);
  assert.equal((await call(repo, "insights", { scribase_org_id: "org_an", app: "other" }, { extra })).body.available, false);
  // Without the analytics slot in the add-on, no app and no entry.
  await call(repo, "orgs", { scribase_org_id: "org_noan", name: "N", addon: addon(true, ["deploy"]) }, { extra });
  assert.equal((await call(repo, "sites", { scribase_org_id: "org_noan", app: "shop" }, { extra })).status, 402);
  assert.equal((await call(repo, "sites", { scribase_org_id: "org_an", app: "!!" }, { extra })).status, 400);
  // A lapsed add-on rewrites the entries as paused; data is kept.
  await call(repo, "orgs", { scribase_org_id: "org_an", name: "An", addon: addon(false) }, { extra });
  assert.equal(sites.entries.get(`site:${r.body.site_key}`)?.paused, true);
  assert.equal((await repo.listApps(orgId)).length, 1);
});

test("ai slot (PY1.4): one key per project for the control plane only, a one-time connect link, BYOK proof and usage", async () => {
  const repo = new MemoryRepo(() => T0);
  await call(repo, "orgs", { scribase_org_id: "org_ai", name: "AI", addon: addon() });
  const first = await call(repo, "ai", { scribase_org_id: "org_ai", project: "shop-prod" });
  assert.equal(first.status, 200, JSON.stringify(first.body));
  assert.match(String(first.body.key), /^ry_ai_/);
  assert.equal(first.body.gateway_url, "https://railyard.ereboslab.com/api/v1/ai");
  assert.match(String(first.body.connect_url), /^https:\/\/railyard\.ereboslab\.com\/connect\/ai\?t=ry_ct_/);
  assert.deepEqual(first.body.providers, []);
  const again = await call(repo, "ai", { scribase_org_id: "org_ai", project: "shop-prod" });
  assert.equal(again.body.key, null, "the key is shown once; Scribase keeps it");
  const rotated = await call(repo, "ai", { scribase_org_id: "org_ai", project: "shop-prod", rotate: true });
  assert.match(String(rotated.body.key), /^ry_ai_/);
  const orgId = (await repo.findScribaseBinding("org_ai"))!.org_id;
  assert.equal((await repo.listAiKeys(orgId)).filter((k) => !k.revoked_at).length, 1, "rotation revokes the old key");
  const waiting = await call(repo, "proof", { scribase_org_id: "org_ai", slot: "ai" });
  assert.equal(waiting.body.state, "no_proof_yet");
  await repo.saveAiCredential({ org_id: orgId, provider: "anthropic", nonce: "n", ciphertext: "c", key_hint: "...1234" });
  const proof = await call(repo, "proof", { scribase_org_id: "org_ai", slot: "ai" });
  assert.equal(proof.body.state, "live");
  assert.equal((proof.body.proof as { id: string }).id, "anthropic");
  const usage = await call(repo, "ai_usage", { scribase_org_id: "org_ai", project: "shop-prod" });
  assert.equal(usage.status, 200);
  assert.deepEqual(Object.keys(usage.body).sort(), ["keys", "providers", "traces"]);
  assert.ok(!JSON.stringify(usage.body).includes("ry_ai_"), "the tab never sees a key");
  assert.deepEqual(usage.body.providers, [{ provider: "anthropic", hint: "...1234", updated_at: T0.toISOString() }]);
  await call(repo, "orgs", { scribase_org_id: "org_noai", name: "N", addon: addon(true, ["deploy"]) });
  assert.equal((await call(repo, "ai", { scribase_org_id: "org_noai", project: "x" })).status, 402);
});
