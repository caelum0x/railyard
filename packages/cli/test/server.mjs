// A local console for CLI tests: the real console device-flow, token and
// deploy-record code (apps/console) over HTTP, on an in-memory repository.
import { createServer } from "node:http";

import { MemoryRepo } from "../../../apps/console/lib/repo.ts";
import { applyLadder } from "../../../apps/console/lib/ladder.ts";
import { emptyEntitlement } from "../../../apps/console/lib/types.ts";
import { approveDevice, createApprovedDevice, orgForBearer, pollDevice, startDevice } from "../../../apps/console/lib/tokens.ts";
import { listDeploys, parseDeployInput, recordDeploy } from "../../../apps/console/app/api/v1/deploys/service.ts";

export async function startConsole() {
  const repo = new MemoryRepo();
  const org = await repo.createOrgForUser({ userId: "u1", email: "founder@example.com", name: "founder" });
  const now = new Date();
  await repo.saveEntitlement(applyLadder(emptyEntitlement(org.id, now), { kind: "card_check_paid", paymentId: "p1", customerId: null, at: now }).ent);
  const seen = [];
  const server = createServer(async (req, res) => {
    let raw = "";
    for await (const c of req) raw += c;
    seen.push({ method: req.method, url: req.url, auth: req.headers.authorization ?? null, body: raw });
    const send = (status, body) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(JSON.stringify(body));
    };
    const url = new URL(req.url, "http://x");
    const base = `http://127.0.0.1:${server.address().port}`;
    if (req.method === "POST" && url.pathname === "/api/v1/device/code") {
      const d = await startDevice(repo, base, new Date());
      // The signed-in user approves the code in the browser.
      setTimeout(() => approveDevice(repo, d.user_code, org.id, "u1", new Date()), 20);
      return send(200, { ...d, interval: 0.01 });
    }
    if (req.method === "POST" && url.pathname === "/api/v1/device/token") {
      const r = await pollDevice(repo, JSON.parse(raw).device_code, new Date());
      return send(r.status, r.body);
    }
    const who = await orgForBearer(repo, req.headers.authorization ?? null);
    if (!who) return send(401, { error: "unauthorized" });
    if (url.pathname === "/api/v1/deploys" && req.method === "POST") {
      const input = parseDeployInput(JSON.parse(raw));
      if (typeof input === "string") return send(400, { error: input });
      const r = await recordDeploy(repo, who.orgId, input, new Date());
      return send(r.status, r.body);
    }
    if (url.pathname === "/api/v1/deploys" && req.method === "GET") {
      const r = await listDeploys(repo, who.orgId, url.searchParams.get("app"), url.searchParams.get("branch"));
      return send(r.status, r.body);
    }
    send(404, { error: "not found" });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  // A one-time claim, as the Scribase bridge issues it (PY1.0).
  const claim = () => createApprovedDevice(repo, org.id, "u1", 120, new Date());
  return { url: `http://127.0.0.1:${server.address().port}`, repo, org, seen, claim, close: () => new Promise((r) => server.close(r)) };
}
