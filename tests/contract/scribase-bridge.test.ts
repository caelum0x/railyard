// Contract test for the Scribase bridge (Wave P PY1.0, RYP-E6). Runs every case
// of fixtures/scribase-bridge/contract.json, in order, against the Railyard
// handler with signed requests. The Scribase side (CA/adapters/railyard/
// bridge_client.rs) tests its requests and parsing against the same file.
//   node --test tests/contract/scribase-bridge.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { MemoryRepo } from "../../apps/console/lib/repo.ts";
import { sign } from "../../apps/console/lib/standard-webhooks.ts";
import { pollDevice } from "../../apps/console/lib/tokens.ts";
import { handleBridge } from "../../apps/console/app/api/v1/bridge/scribase/service.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const contract = JSON.parse(readFileSync(join(HERE, "fixtures/scribase-bridge/contract.json"), "utf8"));
const SECRET = `whsec_${Buffer.from("contract-fixture-secret-32-bytes").toString("base64")}`;
const NOW = new Date("2026-09-29T12:00:00Z");

function matches(actual: unknown, expected: unknown): boolean {
  if (expected && typeof expected === "object" && !Array.isArray(expected)) {
    return !!actual && typeof actual === "object" && Object.entries(expected).every(([k, v]) => matches((actual as Record<string, unknown>)[k], v));
  }
  return JSON.stringify(actual) === JSON.stringify(expected);
}

async function send(repo: MemoryRepo, route: string, payload: unknown, id: string, secret = SECRET) {
  const body = JSON.stringify(payload);
  const ts = String(Math.floor(NOW.getTime() / 1000));
  return handleBridge({ repo, secret: SECRET, consoleUrl: "https://railyard.example", now: () => NOW }, route as never, { id, timestamp: ts, signature: await sign(secret, id, ts, body) }, body);
}

test("every contract case answers with its status and keys, in order", async () => {
  const repo = new MemoryRepo(() => NOW);
  let i = 0;
  for (const c of contract.cases) {
    const r = await send(repo, c.route, c.request, `contract-${++i}`);
    assert.equal(r.status, c.status, c.name);
    assert.deepEqual(Object.keys(r.body).sort(), c.response_keys, c.name);
    if (c.response_match) assert.ok(matches(r.body, c.response_match), `${c.name}: ${JSON.stringify(r.body)}`);
    if (c.route === "claims" && r.status === 200) {
      const t = await pollDevice(repo, r.body.claim as string, NOW);
      assert.equal(t.status, 200, "the claim is exchanged once by the Scribase CLI");
    }
    if (c.route === "proof") assert.ok(contract.slot_state.states.includes(r.body.state), c.name);
  }
});

test("an unsigned or wrongly signed request and a replayed id are refused as the contract says", async () => {
  const repo = new MemoryRepo(() => NOW);
  const other = `whsec_${Buffer.from("not-the-contract-secret-32-bytes").toString("base64")}`;
  const bad = await send(repo, "orgs", contract.cases[0].request, "u-1", other);
  assert.equal(bad.status, contract.unsigned.status);
  assert.deepEqual(Object.keys(bad.body), contract.unsigned.response_keys);
  await send(repo, "orgs", contract.cases[0].request, "r-1");
  const again = await send(repo, "orgs", contract.cases[0].request, "r-1");
  assert.equal(again.status, contract.replayed_id.status);
});

test("the signature vector both sides test against", async () => {
  const v = contract.signature_vector;
  assert.equal(await sign(v.secret, v.id, v.timestamp, v.body), v.signature);
});
