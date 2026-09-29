// Deploy records (RYP-D4 acceptance 3): stored per org, validated, plan-gated.
import { test } from "node:test";
import assert from "node:assert/strict";

import { applyLadder } from "../../../../lib/ladder.ts";
import { MemoryRepo } from "../../../../lib/repo.ts";
import { emptyEntitlement } from "../../../../lib/types.ts";
import { listDeploys, parseDeployInput, recordDeploy } from "./service.ts";

const now = new Date("2026-09-29T12:00:00Z");
async function orgWithTrial(repo: MemoryRepo, user: string) {
  const org = await repo.createOrgForUser({ userId: user, email: `${user}@x.com`, name: user });
  await repo.saveEntitlement(applyLadder(emptyEntitlement(org.id, now), { kind: "card_check_paid", paymentId: `p-${user}`, customerId: null, at: now }).ent);
  return org;
}
const input = (over: Record<string, unknown> = {}) =>
  parseDeployInput({ app: "shop", url: "https://shop-feat-x.acct.workers.dev", commit_sha: "a1b2c3d4", branch: "feat/x", framework: "vite", version_id: "11111111-2222", duration_ms: 4200, status: "succeeded", ...over });

test("a deploy record stores url, commit, branch, framework, duration and status; the app is created on first deploy", async () => {
  const repo = new MemoryRepo(() => now);
  const org = await orgWithTrial(repo, "a");
  const i = input();
  assert.notEqual(typeof i, "string");
  const r = await recordDeploy(repo, org.id, i as Exclude<typeof i, string>, now);
  assert.equal(r.status, 201);
  const d = (r.body.deploy as Record<string, unknown>);
  assert.deepEqual([d.app, d.url, d.commit_sha, d.branch, d.framework, d.duration_ms, d.status], ["shop", "https://shop-feat-x.acct.workers.dev", "a1b2c3d4", "feat/x", "vite", 4200, "succeeded"]);
  assert.equal((await repo.listApps(org.id)).length, 1);
});

test("org B cannot list or roll back onto org A's deploys", async () => {
  const repo = new MemoryRepo(() => now);
  const a = await orgWithTrial(repo, "a");
  const b = await orgWithTrial(repo, "b");
  const r = await recordDeploy(repo, a.id, input() as never, now);
  const id = (r.body.deploy as { id: string }).id;
  assert.equal((await listDeploys(repo, b.id, "shop", null)).body.deploys.length, 0);
  assert.equal((await recordDeploy(repo, b.id, input({ rollback_of: id }) as never, now)).status, 404);
  assert.equal((await listDeploys(repo, a.id, "shop", "feat/x")).body.deploys.length, 1);
});

test("no card or a paused org cannot record deploys", async () => {
  const repo = new MemoryRepo(() => now);
  const org = await repo.createOrgForUser({ userId: "c", email: "c@x.com", name: "c" });
  assert.equal((await recordDeploy(repo, org.id, input() as never, now)).status, 402);
});

test("input is validated", () => {
  assert.equal(input({ framework: "remix" }), "unsupported framework");
  assert.equal(input({ url: "http://x.workers.dev" }), "url must be https");
  assert.equal(input({ commit_sha: "not a sha" }), "invalid commit");
  assert.equal(parseDeployInput(null), "invalid body");
});

test("a folder name like 'My Shop' records every deploy under the one app my-shop", async () => {
  const repo = new MemoryRepo(() => now);
  const org = await orgWithTrial(repo, "d");
  for (let i = 0; i < 2; i += 1) {
    const r = await recordDeploy(repo, org.id, input({ app: "My Shop" }) as never, now);
    assert.equal(r.status, 201, JSON.stringify(r.body));
  }
  assert.deepEqual((await repo.listApps(org.id)).map((a) => a.name), ["my-shop"]);
  assert.equal((await listDeploys(repo, org.id, "My Shop", null)).body.deploys.length, 2);
  assert.equal((await recordDeploy(repo, org.id, input({ rollback_of: "not-a-uuid" }) as never, now)).status, 404);
});
