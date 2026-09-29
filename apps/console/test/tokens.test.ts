// Device-flow login for the CLI and org isolation of bearer tokens.
import { test } from "node:test";
import assert from "node:assert/strict";

import { MemoryRepo } from "../lib/repo.ts";
import { approveDevice, orgForBearer, pollDevice, startDevice } from "../lib/tokens.ts";

test("device flow: pending until approved, one token, then the code is spent", async () => {
  let now = new Date("2026-09-29T12:00:00Z");
  const repo = new MemoryRepo(() => now);
  const org = await repo.createOrgForUser({ userId: "u1", email: "a@x.com", name: "a" });
  const d = await startDevice(repo, "https://c.example", now);
  assert.match(d.user_code, /^[A-Z]{4}-[A-Z]{4}$/);
  assert.ok(!JSON.stringify(repo.devices).includes(d.device_code), "only the hash is stored");
  assert.deepEqual((await pollDevice(repo, d.device_code, now)).body, { error: "authorization_pending" });
  assert.equal(await approveDevice(repo, d.user_code.toLowerCase(), org.id, "u1", now), true);
  const ok = await pollDevice(repo, d.device_code, now);
  assert.equal(ok.status, 200);
  const token = (ok.body as { access_token: string }).access_token;
  assert.ok(!JSON.stringify(repo.tokens).includes(token), "only the hash is stored");
  assert.deepEqual(await orgForBearer(repo, `Bearer ${token}`), { orgId: org.id, userId: "u1" });
  assert.deepEqual((await pollDevice(repo, d.device_code, now)).body, { error: "invalid_grant" });
  now = new Date(now.getTime() + 601_000);
  const late = await startDevice(repo, "https://c.example", new Date("2026-09-29T11:00:00Z"));
  assert.equal(await approveDevice(repo, late.user_code, org.id, "u1", now), false, "expired codes cannot be approved");
});

test("a token for org A resolves only to org A; junk is refused", async () => {
  const repo = new MemoryRepo();
  const a = await repo.createOrgForUser({ userId: "ua", email: "a@x.com", name: "a" });
  const b = await repo.createOrgForUser({ userId: "ub", email: "b@x.com", name: "b" });
  const now = new Date();
  const da = await startDevice(repo, "https://c", now);
  await approveDevice(repo, da.user_code, a.id, "ua", now);
  const ta = ((await pollDevice(repo, da.device_code, now)).body as { access_token: string }).access_token;
  assert.equal((await orgForBearer(repo, `Bearer ${ta}`))?.orgId, a.id);
  assert.notEqual((await orgForBearer(repo, `Bearer ${ta}`))?.orgId, b.id);
  assert.equal(await orgForBearer(repo, "Bearer ry_notarealtokenbutlongenough"), null);
  assert.equal(await orgForBearer(repo, null), null);
  assert.equal(await orgForBearer(repo, ta), null, "scheme required");
});
