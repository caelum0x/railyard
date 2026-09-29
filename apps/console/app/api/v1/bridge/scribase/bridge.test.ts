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
async function call(repo: MemoryRepo, route: "orgs" | "claims" | "proof", payload: unknown, opts: { at?: Date; secret?: string; id?: string } = {}) {
  const at = opts.at ?? T0;
  const body = JSON.stringify(payload);
  const id = opts.id ?? `msg_${++n}`;
  const ts = String(Math.floor(at.getTime() / 1000));
  const signature = await sign(opts.secret ?? SECRET, id, ts, body);
  return handleBridge(
    { repo, secret: SECRET, consoleUrl: "https://railyard.ereboslab.com", now: () => at },
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
  assert.ok(files.length >= 4, "service and three routes");
  for (const f of files) {
    const src = readFileSync(f, "utf8");
    assert.doesNotMatch(src, /console\.(log|info|warn|error|debug)|process\.(stdout|stderr)/, f);
  }
});
