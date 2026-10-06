// A23: destination transforms. A destination receives only the events its
// `where` keeps, with properties reshaped by keep/drop, rename and set; the
// cursor still moves past every row read; a transform can be replaced or
// removed later; bad transforms are refused with 400.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomBytes } from "node:crypto";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { CURSOR_LAG_MS, runExports, type ExportedEvent } from "../../src/exports/runner.ts";
import { applyTransform, checkTransform } from "../../src/exports/transform.ts";
import { silentLogger } from "../../src/log.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { api, insertEvents, SECRET, type Row } from "../a3/helpers.ts";

process.env.PA_EXPORTS_KEY = randomBytes(32).toString("base64");
process.env.PA_EXPORTS_ALLOW_PRIVATE = "1";

let pg: ScratchPg;
let run: Running;
let base = "";
const bodies: string[] = [];
const site = makeSite({ name: "shop" });
let now = new Date(Math.ceil(Date.now() / 3600_000) * 3600_000);
const later = () => (now = new Date(now.getTime() + CURSOR_LAG_MS + 3600_000));

before(async () => {
  if (skipNoPg) return;
  const srv = createServer((req, res) => {
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", () => {
      bodies.push(body);
      res.writeHead(200).end("{}");
    });
  });
  await new Promise<void>((r) => srv.listen(0, "127.0.0.1", () => r()));
  base = `http://127.0.0.1:${(srv.address() as AddressInfo).port}`;
  pg = await migratedPg();
  run = await startService(pg, [site], { consoleSecret: SECRET, now: () => now });
});

const e = (event: string, properties: Record<string, unknown>): ExportedEvent => ({
  uuid: `u-${event}-${JSON.stringify(properties)}`,
  event,
  distinct_id: "d1",
  person_id: "p1",
  timestamp: "2026-10-06T10:00:00.000Z",
  properties,
  elements_chain: null,
  session_id: null,
});

test("where keeps matching events; keep/drop, rename and set reshape without mutating", () => {
  const events = [e("purchase", { plan: "pro", amount: 30, email: "a@x.io" }), e("purchase", { plan: "free", amount: 0 }), e("signup", { plan: "pro" })];
  const t = checkTransform({ where: [{ property: "event", op: "eq", value: "purchase" }, { property: "amount", op: "gt", value: 10 }], drop: ["email"], rename: { amount: "revenue" }, set: { source: "railyard" } });
  const out = applyTransform(t, events);
  assert.deepEqual(out.map((x) => x.properties), [{ plan: "pro", revenue: 30, source: "railyard" }]);
  assert.equal(events[0].properties.email, "a@x.io", "the input events are untouched");
  const kept = applyTransform(checkTransform({ keep: ["plan"], where: [{ property: "plan", op: "in", value: ["pro", "team"] }] }), events);
  assert.deepEqual(kept.map((x) => x.properties), [{ plan: "pro" }, { plan: "pro" }]);
  assert.equal(applyTransform(checkTransform({ where: [{ property: "email", op: "exists" }] }), events).length, 1);
  assert.equal(applyTransform(null, events), events);
});

test("bad transforms are refused", () => {
  for (const bad of [[], { where: [] }, { where: [{ property: "x", op: "like", value: 1 }] }, { where: [{ property: "x", op: "gt", value: "a" }] }, { keep: ["a"], drop: ["b"] }, { rename: { a: 1 } }, { set: { a: { nested: 1 } } }, { run: "code" }]) {
    assert.throws(() => checkTransform(bad), /transform|Condition/i, JSON.stringify(bad));
  }
  assert.equal(checkTransform(null), null);
});

test("a webhook destination receives only what its transform keeps; the transform can be replaced and removed", { skip: skipNoPg }, async () => {
  const made = await api(run, site, "/exports", {
    now,
    body: { name: "pro-only", kind: "webhook", start: "all", config: { url: `${base}/hook` }, transform: { where: [{ property: "plan", op: "eq", value: "pro" }], keep: ["plan"] } },
  });
  assert.equal(made.status, 201, JSON.stringify(made.body));
  assert.deepEqual(made.body.destination.config.transform.keep, ["plan"]);
  const rows: Row[] = [
    { event: "purchase", distinct_id: "a", timestamp: new Date(now.getTime() - 1000), properties: { plan: "pro", email: "a@x.io" } },
    { event: "purchase", distinct_id: "b", timestamp: new Date(now.getTime() - 1000), properties: { plan: "free" } },
  ];
  await insertEvents(pg, site.project_id, rows);
  bodies.length = 0;
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  assert.equal(bodies.length, 1);
  const sent = JSON.parse(bodies[0]).events;
  assert.deepEqual(sent.map((x: { distinct_id: string; properties: unknown }) => [x.distinct_id, x.properties]), [["a", { plan: "pro" }]]);

  // Only filtered-out events: nothing is sent, the cursor still moves.
  await insertEvents(pg, site.project_id, [{ event: "purchase", distinct_id: "c", timestamp: new Date(now.getTime() + 1000), properties: { plan: "free" } }]);
  bodies.length = 0;
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  assert.equal(bodies.length, 0);
  const d1 = (await api(run, site, `/exports/${made.body.destination.id}`, { now })).body.destination;
  assert.equal(d1.status, "ok");

  // Replace, then remove the transform.
  const put = await api(run, site, `/exports/${made.body.destination.id}`, { now, method: "PUT", body: { transform: { set: { source: "rail" } } } });
  assert.equal(put.status, 200, JSON.stringify(put.body));
  assert.deepEqual(put.body.destination.config.transform, { set: { source: "rail" } });
  assert.equal((await api(run, site, `/exports/${made.body.destination.id}`, { now, method: "PUT", body: { transform: { where: [] } } })).status, 400);
  const cleared = await api(run, site, `/exports/${made.body.destination.id}`, { now, method: "PUT", body: { transform: null } });
  assert.equal(cleared.body.destination.config.transform, undefined);
  assert.equal(cleared.body.destination.config.url, `${base}/hook`, "the rest of the config stays");
});
