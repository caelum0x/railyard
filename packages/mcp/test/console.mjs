// Local console for MCP tests: the real console token and deploy code over
// HTTP, two orgs with their own tokens and deploys.
import { createServer } from "node:http";

import { MemoryRepo } from "../../../apps/console/lib/repo.ts";
import { access, applyLadder } from "../../../apps/console/lib/ladder.ts";
import { emptyEntitlement } from "../../../apps/console/lib/types.ts";
import { approveDevice, orgForBearer, pollDevice, startDevice } from "../../../apps/console/lib/tokens.ts";
import { listDeploys, parseDeployInput, recordDeploy } from "../../../apps/console/app/api/v1/deploys/service.ts";

async function orgWithToken(repo, user) {
  const now = new Date();
  const org = await repo.createOrgForUser({ userId: user, email: `${user}@example.com`, name: user });
  await repo.saveEntitlement(applyLadder(emptyEntitlement(org.id, now), { kind: "card_check_paid", paymentId: `p-${user}`, customerId: null, at: now }).ent);
  const d = await startDevice(repo, "http://c", now);
  await approveDevice(repo, d.user_code, org.id, user, now);
  const token = (await pollDevice(repo, d.device_code, now)).body.access_token;
  return { org, token };
}

export async function startConsole() {
  const repo = new MemoryRepo();
  const a = await orgWithToken(repo, "alice");
  const b = await orgWithToken(repo, "bob");
  const now = new Date();
  const rec = (orgId, app, branch, url) =>
    recordDeploy(repo, orgId, parseDeployInput({ app, url, commit_sha: "abcdef1", branch, framework: "nextjs", version_id: "v1", duration_ms: 1000, status: "succeeded" }), now);
  await rec(a.org.id, "alice-shop", "main", "https://alice-shop.a.workers.dev");
  await rec(b.org.id, "bob-app", "main", "https://bob-app.b.workers.dev");
  const server = createServer(async (req, res) => {
    const send = (status, body) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(JSON.stringify(body));
    };
    const who = await orgForBearer(repo, req.headers.authorization ?? null);
    if (!who) return send(401, { error: "unauthorized" });
    const url = new URL(req.url, "http://x");
    if (url.pathname === "/api/v1/deploys") {
      const r = await listDeploys(repo, who.orgId, url.searchParams.get("app"), url.searchParams.get("branch"));
      return send(r.status, r.body);
    }
    if (url.pathname === "/api/v1/me") {
      const org = await repo.getOrg(who.orgId);
      const apps = await repo.listApps(who.orgId);
      return send(200, { org: { id: org.id, name: org.name }, access: access(await repo.getEntitlement(who.orgId), new Date()), apps: apps.map((x) => ({ id: x.id, name: x.name })) });
    }
    send(404, { error: "not found" });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  return { url: `http://127.0.0.1:${server.address().port}`, a, b, close: () => new Promise((r) => server.close(r)) };
}
