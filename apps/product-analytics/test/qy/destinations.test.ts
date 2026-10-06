// QY1.4 (A23): the destination catalogue. Slack, Discord and HubSpot each get
// the project's new events in their own shape, with Standard Webhooks
// signature headers under the destination's signing secret (shown once),
// retries with backoff and a run log, and an on/off switch. A destination
// never receives another project's rows, and its secret (the webhook address
// or the token) is never read back.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { verify } from "../../src/bridge/standard-webhooks.ts";
import { CURSOR_LAG_MS, runExports } from "../../src/exports/runner.ts";
import { checkCatalogue, discordBody, hubspotInputs, slackBody } from "../../src/exports/catalogue.ts";
import { silentLogger } from "../../src/log.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { api, insertEvents, SECRET, type Row } from "../a3/helpers.ts";

process.env.PA_EXPORTS_KEY = randomBytes(32).toString("base64");
process.env.PA_EXPORTS_ALLOW_PRIVATE = "1";

interface Got {
  path: string;
  headers: Record<string, string>;
  body: string;
}
let pg: ScratchPg;
let run: Running;
let base = "";
const got: Got[] = [];
let failNext = 0;
const site = makeSite({ name: "shop" });
const other = makeSite({ name: "other" });
// The runner compares its clock with inserted_at, which the database stamps
// with its own clock, so the test clock starts at the real hour (a fixed date
// made this test pass only on the day it was written).
let now = new Date(Math.ceil(Date.now() / 3600_000) * 3600_000);
const later = () => (now = new Date(now.getTime() + CURSOR_LAG_MS + 3600_000));

before(async () => {
  if (skipNoPg) return;
  const srv = createServer((req, res) => {
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", () => {
      if (failNext > 0) {
        failNext -= 1;
        return void res.writeHead(500).end();
      }
      got.push({ path: req.url ?? "", headers: Object.fromEntries(Object.entries(req.headers).map(([k, v]) => [k, String(v)])), body });
      res.writeHead(200, { "content-type": "application/json" }).end("{}");
    });
  });
  await new Promise<void>((r) => srv.listen(0, "127.0.0.1", () => r()));
  base = `http://127.0.0.1:${(srv.address() as AddressInfo).port}`;
  process.env.PA_HUBSPOT_URL = `${base}/hubspot`;
  pg = await migratedPg();
  run = await startService(pg, [site, other], { consoleSecret: SECRET, now: () => now });
});

const ev = (n: number, prefix: string, at: Date): Row[] =>
  Array.from({ length: n }, (_, i) => ({ event: "signup", distinct_id: `${prefix}${i}`, timestamp: at, properties: { $current_url: `https://shop.example/p${i}`, $set: { email: `${prefix}${i}@shop.example`, firstname: `F${i}` } } }));

test("message shapes: Slack {text, blocks}, Discord {content} without mentions, HubSpot contact upserts by email", () => {
  const events = Array.from({ length: 25 }, (_, i) => ({ uuid: `u${i}`, event: "signup", distinct_id: `d${i}`, person_id: "p", timestamp: "2026-10-05T12:00:00.000Z", properties: { $set: { email: i % 2 ? `A${i}@Shop.example` : "bad", company: "Acme" } }, elements_chain: null, session_id: null }));
  const s = slackBody(events);
  assert.match(s.text, /^25 new events: signup by d0/);
  assert.match(JSON.stringify(s.blocks), /and 5 more/);
  const d = discordBody(events);
  assert.ok(d.content.length <= 2000);
  assert.deepEqual(d.allowed_mentions, { parse: [] });
  const h = hubspotInputs(events);
  assert.equal(h.length, 12, "only valid addresses");
  assert.deepEqual(h[0], { idProperty: "email", id: "a1@shop.example", properties: { email: "a1@shop.example", company: "Acme" } });
  // Outside tests, only the services' own webhook hosts are accepted.
  const prev = process.env.PA_EXPORTS_ALLOW_PRIVATE;
  delete process.env.PA_EXPORTS_ALLOW_PRIVATE;
  try {
    assert.throws(() => checkCatalogue("slack", {}, "https://evil.example/services/x"), /hooks\.slack\.com/);
    assert.equal(checkCatalogue("slack", {}, "https://hooks.slack.com/services/T/B/x").bundle.target, "https://hooks.slack.com/services/T/B/x");
    assert.throws(() => checkCatalogue("discord", {}, "https://discord.com/other"), /discord\.com/);
    assert.throws(() => checkCatalogue("hubspot", {}, "has spaces"), /token/);
  } finally {
    process.env.PA_EXPORTS_ALLOW_PRIVATE = prev;
  }
});

test("Slack, Discord and HubSpot receive this project's events, signed, and never the other project's", { skip: skipNoPg }, async () => {
  const made: Record<string, { id: string; signing: string }> = {};
  for (const [kind, secret] of [["slack", `${base}/slack`], ["discord", `${base}/discord`], ["hubspot", "pat-na1-abcdefghijklmnop"]] as const) {
    const r = await api(run, site, "/exports", { now, body: { name: kind, kind, secret, start: "all", config: { label: `#${kind}` } } });
    assert.equal(r.status, 201, JSON.stringify(r.body));
    assert.match(r.body.signing_secret, /^whsec_/);
    assert.ok(!JSON.stringify(r.body.destination).includes(base) && !JSON.stringify(r.body.destination).includes("pat-na1"), "the secret is not in the destination");
    made[kind] = { id: r.body.destination.id, signing: r.body.signing_secret };
  }
  const listed = JSON.stringify((await api(run, site, "/exports", { now })).body);
  assert.ok(!listed.includes("pat-na1") && !listed.includes("/slack"), "never read back");
  await insertEvents(pg, site.project_id, ev(3, "s", new Date(now.getTime() - 1000)));
  await insertEvents(pg, other.project_id, ev(4, "leak", new Date(now.getTime() - 1000)));
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  const byPath = (p: string) => got.filter((g) => g.path.startsWith(p));
  const slack = byPath("/slack");
  const discord = byPath("/discord");
  const hubspot = byPath("/hubspot/crm/v3/objects/contacts/batch/upsert");
  assert.equal(slack.length, 1);
  assert.equal(discord.length, 1);
  assert.equal(hubspot.length, 1);
  assert.match(JSON.parse(slack[0].body).text, /^3 new events/);
  assert.match(JSON.parse(discord[0].body).content, /^3 new events/);
  assert.deepEqual(JSON.parse(hubspot[0].body).inputs.map((i: { id: string }) => i.id).sort(), ["s0@shop.example", "s1@shop.example", "s2@shop.example"]);
  assert.equal(hubspot[0].headers.authorization, "Bearer pat-na1-abcdefghijklmnop");
  for (const [g, kind] of [[slack[0], "slack"], [discord[0], "discord"], [hubspot[0], "hubspot"]] as const) {
    assert.ok(verify(made[kind].signing, { id: g.headers["webhook-id"], timestamp: g.headers["webhook-timestamp"], signature: g.headers["webhook-signature"] }, g.body, Math.floor(now.getTime() / 1000)), `${kind} is signed`);
  }
  assert.ok(!got.some((g) => g.body.includes("leak")), "no other project's rows");
});

test("a failed delivery is retried with backoff and logged; a switched-off destination waits", { skip: skipNoPg }, async () => {
  const r = await api(run, site, "/exports", { now, body: { name: "retry", kind: "discord", start: "all", secret: `${base}/discord-retry` } });
  const id = r.body.destination.id;
  // Pause the others so only this one sends.
  for (const d of (await api(run, site, "/exports", { now })).body.destinations) if (d.id !== id) await api(run, site, `/exports/${d.id}`, { now, method: "PUT", body: { enabled: false } });
  await insertEvents(pg, site.project_id, ev(2, "r", new Date(now.getTime() - 1000)));
  failNext = 1;
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  let d = (await api(run, site, `/exports/${id}`, { now })).body.destination;
  assert.equal(d.status, "retrying");
  assert.equal(d.attempts, 1);
  const runs = (await api(run, site, `/exports/${id}/runs`, { now })).body.runs;
  assert.equal(runs[0].status, "failed");
  assert.equal(runs[0].http_status, 500);
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  d = (await api(run, site, `/exports/${id}`, { now })).body.destination;
  assert.equal(d.status, "ok");
  const delivered = got.filter((g) => g.path === "/discord-retry");
  assert.equal(delivered.length, 1);
  assert.match(JSON.parse(delivered[0].body).content, /^5 new events[\s\S]*signup by r0/, "everything kept since the start, after one failed try");
  // Off: nothing is sent; on again: the waiting events go.
  await api(run, site, `/exports/${id}`, { now, method: "PUT", body: { enabled: false } });
  await insertEvents(pg, site.project_id, ev(1, "off", new Date(now.getTime() - 1000)));
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  assert.equal(got.filter((g) => g.path === "/discord-retry").length, 1);
  await api(run, site, `/exports/${id}`, { now, method: "PUT", body: { enabled: true, retry_now: true } });
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  assert.equal(got.filter((g) => g.path === "/discord-retry").length, 2);
});
