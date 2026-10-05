// A fake of the Cloudflare API calls custom domains make (QY1.5): the OAuth
// token endpoint, accounts, zones, Workers custom domains and certificate
// packs. It records every call with the token it carried.
import { createServer } from "node:http";

export async function startCloudflare({ zones = ["example.com"], account = "a".repeat(32), tokens = ["cf-access-1", "cf-local-token-123"] } = {}) {
  const calls = [];
  const domains = new Map(); // id -> {id, hostname, service, zone_id, zone_name}
  let n = 0;
  const zoneRows = zones.map((name, i) => ({ id: `zone${i}${"0".repeat(27)}`, name }));
  const server = createServer(async (req, res) => {
    let raw = "";
    for await (const c of req) raw += c;
    const url = new URL(req.url, "http://x");
    const auth = req.headers.authorization ?? null;
    calls.push({ method: req.method, path: url.pathname, query: url.search, auth, body: raw });
    const send = (status, body) => res.writeHead(status, { "content-type": "application/json" }).end(JSON.stringify(body));
    const ok = (result) => send(200, { success: true, errors: [], result });
    if (url.pathname === "/oauth2/token") return send(200, { access_token: "cf-access-1", refresh_token: "cf-refresh-2", expires_in: 3600 });
    const token = (auth ?? "").replace(/^Bearer /, "");
    if (!tokens.includes(token)) return send(403, { success: false, errors: [{ code: 10000, message: "Authentication error" }] });
    const p = url.pathname.replace(/^\/client\/v4/, "");
    if (p === "/accounts") return ok([{ id: account, name: "Shop" }]);
    if (p === "/zones") return ok(zoneRows.filter((z) => z.name === url.searchParams.get("name")));
    const wd = new RegExp(`^/accounts/${account}/workers/domains(?:/([^/]+))?$`).exec(p);
    if (wd && req.method === "GET") return ok([...domains.values()].filter((d) => !url.searchParams.get("service") || d.service === url.searchParams.get("service")));
    if (wd && req.method === "PUT") {
      const b = JSON.parse(raw);
      const zone = zoneRows.find((z) => z.id === b.zone_id);
      if (!zone || !b.hostname.endsWith(zone.name)) return send(400, { success: false, errors: [{ code: 100117, message: "hostname is not in the zone" }] });
      const d = { id: `dom${++n}`, hostname: b.hostname, service: b.service, zone_id: zone.id, zone_name: zone.name, environment: b.environment };
      domains.set(d.id, d);
      return ok(d);
    }
    if (wd && req.method === "DELETE") {
      if (!domains.delete(wd[1])) return send(404, { success: false, errors: [{ code: 100100, message: "not found" }] });
      return ok(null);
    }
    const cp = /^\/zones\/([^/]+)\/ssl\/certificate_packs$/.exec(p);
    if (cp) return ok([...domains.values()].filter((d) => d.zone_id === cp[1]).map((d, i) => ({ id: `cp${i}`, hosts: [d.hostname], status: d.hostname.startsWith("live.") ? "active" : "pending_validation" })));
    send(404, { success: false, errors: [{ code: 7003, message: "no route" }] });
  });
  await new Promise((r) => server.listen(0, "127.0.0.1", r));
  const base = `http://127.0.0.1:${server.address().port}`;
  return { base, api: `${base}/client/v4`, calls, domains, account, close: () => new Promise((r) => server.close(r)) };
}
