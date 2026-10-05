// QY1.5 (I6): `railyard domains` against the real console code (custom
// domains service and handlers, in-memory store) and a fake Cloudflare API.
// With the org's grant the console attaches the domain; without it the CLI
// uses the local login and the console records the outcome. No token is ever
// printed, and remove is idempotent.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { main } from "../src/cli.mjs";
import { cleanHostname, domainState, zoneCandidates } from "../src/domains.mjs";
import { wranglerConfigPaths } from "../src/domain-commands.mjs";
import { saveCredentials } from "../src/credentials.mjs";
import { startCloudflare } from "./fixtures/fake-cloudflare.mjs";
import { grantCloudflare, startConsole } from "./server.mjs";
import { createApprovedDevice, pollDevice } from "../../../apps/console/lib/tokens.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const TMP = mkdtempSync(join(tmpdir(), "railyard-domains-"));
let cf;
let cons;
let token;

before(async () => {
  cf = await startCloudflare({ zones: ["example.com", "shop.example.org"] });
  cons = await startConsole({ cloudflare: cf });
  // A signed-in CLI: a device code approved for the org, exchanged once.
  const claim = await createApprovedDevice(cons.repo, cons.org.id, "u1", 120, new Date());
  const t = await pollDevice(cons.repo, claim, new Date());
  token = t.body.access_token;
  saveCredentials({ console: cons.url, token, org: cons.org.id }, { RAILYARD_CONFIG_DIR: join(TMP, "config") });
});
after(async () => {
  await cons.close();
  await cf.close();
  rmSync(TMP, { recursive: true, force: true });
});

function ctx(extraEnv = {}) {
  const lines = [];
  const env = {
    PATH: process.env.PATH,
    HOME: TMP,
    RAILYARD_CONFIG_DIR: join(TMP, "config"),
    RAILYARD_CF_API: cf.api,
    RAILYARD_WRANGLER: join(HERE, "fixtures/fake-wrangler.mjs"),
    FAKE_WRANGLER_LOG: join(TMP, "wrangler.log"),
    FAKE_WRANGLER_STATE: join(TMP, "wrangler-state.json"),
    ...extraEnv,
  };
  return { lines, c: { env, cwd: join(TMP, "shop"), out: (s) => lines.push(s), err: (s) => lines.push(`ERR ${s}`), now: () => Date.now(), fetchImpl: fetch } };
}
const printed = (lines) => lines.join("\n");

test("hostnames, zones and certificate states", () => {
  assert.equal(cleanHostname(" App.Example.com. "), "app.example.com");
  for (const bad of ["example", "*.example.com", "x.workers.dev", "https://a.example.com", "a..b.com", ""]) assert.equal(cleanHostname(bad), null, bad);
  assert.deepEqual(zoneCandidates("app.shop.example.org"), ["app.shop.example.org", "shop.example.org", "example.org"]);
  assert.equal(domainState("active"), "active");
  assert.equal(domainState("pending_validation"), "pending");
  assert.equal(domainState(null), "pending");
  assert.equal(domainState("deleted"), "error");
});

test("with the org's grant: add attaches through the console, list shows it, remove detaches, twice is a no-op", async () => {
  await grantCloudflare(cons.ship, cons.org.id, cf.account);
  const add = ctx();
  assert.equal(await main(["domains", "add", "app.example.com", "--app", "shop"], add.c), 0, printed(add.lines));
  assert.match(printed(add.lines), /Attached app\.example\.com to shop: pending/);
  const put = cf.calls.find((c) => c.method === "PUT");
  assert.deepEqual(JSON.parse(put.body), { environment: "production", hostname: "app.example.com", service: "shop", zone_id: `zone0${"0".repeat(27)}` });
  assert.equal(put.auth, "Bearer cf-access-1", "the console's short-lived access token, from the sealed grant");
  // The grant's token never reaches the CLI: no CLI request carried it, and nothing printed it.
  assert.ok(!cons.seen.some((s) => (s.body ?? "").includes("cf-access")));
  const list = ctx();
  assert.equal(await main(["domains", "--app", "shop"], list.c), 0);
  assert.match(printed(list.lines), /app\.example\.com  pending/);
  // Already attached to this Worker: kept, not attached twice.
  assert.equal(await main(["domains", "add", "app.example.com", "--app", "shop"], ctx().c), 0);
  assert.equal(cf.calls.filter((c) => c.method === "PUT").length, 1);
  const rm = ctx();
  assert.equal(await main(["domains", "remove", "app.example.com", "--app", "shop"], rm.c), 0);
  assert.match(printed(rm.lines), /Removed app\.example\.com/);
  assert.equal(cf.domains.size, 0);
  const again = ctx();
  assert.equal(await main(["domains", "remove", "app.example.com", "--app", "shop"], again.c), 0);
  assert.match(printed(again.lines), /was not attached; nothing to remove/);
  for (const l of [...add.lines, ...list.lines, ...rm.lines, ...again.lines]) assert.doesNotMatch(l, /cf-access|cf-refresh|ry_[A-Za-z0-9_-]{20,}/);
});

test("refusals: a hostname outside the account's zones, and a domain already on another Worker", async () => {
  const outside = ctx();
  assert.equal(await main(["domains", "add", "app.other.net", "--app", "shop"], outside.c), 1);
  assert.match(printed(outside.lines), /not in a zone of this Cloudflare account/);
  cf.domains.set("x1", { id: "x1", hostname: "taken.example.com", service: "someone-else", zone_id: `zone0${"0".repeat(27)}`, zone_name: "example.com" });
  const taken = ctx();
  assert.equal(await main(["domains", "add", "taken.example.com", "--app", "shop"], taken.c), 1);
  assert.match(printed(taken.lines), /already points at another Worker/);
  assert.equal(cf.domains.get("x1").service, "someone-else", "left alone");
  cf.domains.delete("x1");
  const bad = ctx();
  assert.equal(await main(["domains", "add", "not a host", "--app", "shop"], bad.c), 2);
});

test("without a grant: the local login attaches, the console records it, and the Deploy tab data shows it", async () => {
  cons.ship.store.cloudflare.clear();
  const add = ctx({ CLOUDFLARE_API_TOKEN: "cf-local-token-123" });
  assert.equal(await main(["domains", "add", "live.shop.example.org", "--app", "shop"], add.c), 0, printed(add.lines));
  assert.match(printed(add.lines), /Attached live\.shop\.example\.org to shop with your Cloudflare login: active/);
  assert.equal(cf.calls.filter((c) => c.method === "PUT").at(-1).auth, "Bearer cf-local-token-123");
  const rows = await cons.ship.store.listDomains(cons.org.id, "shop");
  assert.deepEqual(rows.map((r) => [r.hostname, r.state, r.worker]), [["live.shop.example.org", "active", "shop"]]);
  assert.ok(!cons.seen.some((s) => (s.body ?? "").includes("cf-local-token")), "the local token never goes to the console");
  assert.doesNotMatch(printed(add.lines), /cf-local-token/);
  const rm = ctx({ CLOUDFLARE_API_TOKEN: "cf-local-token-123" });
  assert.equal(await main(["domains", "remove", "live.shop.example.org", "--app", "shop"], rm.c), 0);
  assert.deepEqual(await cons.ship.store.listDomains(cons.org.id, "shop"), []);
});

test("the wrangler login is read from its config file when no API token is set; an error never prints it", async () => {
  const cfgPath = join(TMP, "wrangler", "default.toml");
  mkdirSync(dirname(cfgPath), { recursive: true });
  writeFileSync(cfgPath, 'oauth_token = "cf-local-token-123"\nexpiration_time = "2099-01-01T00:00:00Z"\n');
  assert.deepEqual(wranglerConfigPaths({ RAILYARD_WRANGLER_CONFIG: cfgPath }), [cfgPath]);
  const add = ctx({ RAILYARD_WRANGLER_CONFIG: cfgPath });
  assert.equal(await main(["domains", "add", "www.example.com", "--app", "shop"], add.c), 0, printed(add.lines));
  writeFileSync(cfgPath, 'oauth_token = "cf-wrong-token-456789"\n');
  const refused = ctx({ RAILYARD_WRANGLER_CONFIG: cfgPath });
  assert.equal(await main(["domains", "remove", "www.example.com", "--app", "shop"], refused.c), 1);
  assert.match(printed(refused.lines), /Cloudflare refused/);
  assert.doesNotMatch(printed(refused.lines), /cf-wrong-token/);
  assert.ok(readFileSync(cfgPath, "utf8").includes("cf-wrong"), "the file is only read");
});
