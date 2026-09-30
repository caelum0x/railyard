// Caps, pause and metering: at 100% the SDK still gets 200 and the event is
// counted, not stored; a paused org stores nothing; caps.ts is a byte copy
// of the console's; the meter follows the consumer contract behind
// RAILYARD_LIVE.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { capMessage } from "../../src/caps.ts";
import { METER_CODE, Meter, meterEvent } from "../../src/capture/meter.ts";
import { RAILYARD_EVENTS_LIMIT } from "../../src/plans.ts";
import { silentLogger } from "../../src/log.ts";
import { signConsoleToken } from "../../src/tenancy/console-token.ts";
import { count, makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "./helpers.ts";

const HERE = dirname(fileURLToPath(import.meta.url));
const RY = join(HERE, "../../../..");
const SECRET = "c".repeat(40);

test("caps.ts is a byte-for-byte copy of the console's lib/caps.ts", () => {
  assert.equal(readFileSync(join(HERE, "../../src/caps.ts"), "utf8"), readFileSync(join(RY, "apps/console/lib/caps.ts"), "utf8"));
});

test("the events limit matches the console plan", async () => {
  const { PLAN } = await import(join(RY, "apps/console/lib/plans.ts"));
  assert.equal(RAILYARD_EVENTS_LIMIT, PLAN.eventsLimit);
});

let pg: ScratchPg;
let run: Running;
const meterCalls: unknown[] = [];
const capped = makeSite({ events_limit: 3, scribase_org_id: "org_abc123" });
const paused = makeSite({ paused: true, scribase_org_id: "org_paused1" });
const unbound = makeSite({ events_limit: 1000 });

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [capped, paused, unbound], {
    consoleSecret: SECRET,
    meter: { live: true, url: "http://meter.local", apiKey: "k" },
    meterFetch: (async (_u: string, init: RequestInit) => {
      meterCalls.push(JSON.parse(String(init.body)));
      return new Response("{}", { status: 200 });
    }) as typeof fetch,
  });
});

const batch = (token: string, n: number) =>
  fetch(`${run.base}/batch/`, {
    method: "POST",
    body: JSON.stringify({ api_key: token, batch: Array.from({ length: n }, (_, i) => ({ event: `e${i}`, distinct_id: "u", uuid: randomUUID() })) }),
  });

test("at 100% of the cap the SDK gets 200, events are counted and not stored; the console shows capMessage", { skip: skipNoPg }, async () => {
  assert.equal((await batch(capped.token, 2)).status, 200);
  assert.equal((await batch(capped.token, 2)).status, 200);
  await run.svc.deps.writer.flush();
  assert.equal(count(pg, `project_id = '${capped.project_id}'`), 3);
  assert.equal((await batch(capped.token, 5)).status, 200);
  await run.svc.deps.writer.flush();
  assert.equal(count(pg, `project_id = '${capped.project_id}'`), 3);
  assert.equal(pg.psql(`select received || '|' || stored || '|' || capped from pa.usage_monthly where project_id = '${capped.project_id}'`), "9|3|6");
  const token = await signConsoleToken(SECRET, { org_id: capped.org_id, app_ids: [capped.project_id], role: "owner" }, new Date());
  const status = (await (await fetch(`${run.base}/v1/projects/${capped.project_id}/status`, { headers: { authorization: `Bearer ${token}` } })).json()) as Record<string, unknown>;
  assert.equal(status.cap_message, capMessage(3, 3, new Date()));
  assert.match(String(status.cap_message), /New events are still counted, and are not stored until/);
});

test("a paused org stores nothing and is not metered", { skip: skipNoPg }, async () => {
  assert.equal((await batch(paused.token, 4)).status, 200);
  await run.svc.deps.writer.flush();
  assert.equal(count(pg, `project_id = '${paused.project_id}'`), 0);
  assert.equal(pg.psql(`select count(*) from pa.persons where project_id = '${paused.project_id}'`), "0");
  assert.equal(pg.psql(`select received || '|' || stored || '|' || dropped from pa.usage_monthly where project_id = '${paused.project_id}'`), "4|0|4");
});

test("the meter posts finished minutes in the consumer-contract shape, once per minute and org", { skip: skipNoPg }, async () => {
  await run.svc.deps.meter.flush(new Date(Date.now() + 120_000));
  const contract = JSON.parse(readFileSync(join(RY, "tests/contract/consumers/contract.json"), "utf8"));
  assert.ok(contract.products.scribase.meters.includes(METER_CODE));
  const pattern = new RegExp(contract.transaction_id_pattern);
  const events = meterCalls.map((c) => (c as { event: ReturnType<typeof meterEvent> }).event);
  assert.ok(events.length >= 1);
  for (const e of events) {
    assert.match(e.transaction_id, pattern);
    assert.equal(e.code, METER_CODE);
    assert.equal(e.external_customer_id, "org_abc123");
    assert.equal(e.external_subscription_id, "scribase-portfolio-org_abc123");
    assert.ok(Number.isInteger(e.timestamp));
  }
  const total = events.reduce((n, e) => n + e.properties.events, 0);
  assert.equal(total, 9, "stored + capped of the Scribase-bound org; paused and unbound orgs are not metered");
  assert.equal(new Set(events.map((e) => e.transaction_id)).size, events.length);
});

test("with RAILYARD_LIVE off nothing is sent", async () => {
  const sent: unknown[] = [];
  const m = new Meter({ live: false, url: "http://meter.local", apiKey: "k" }, silentLogger, (async (...a: unknown[]) => (sent.push(a), new Response("{}"))) as typeof fetch);
  m.record("org_abc123", new Date("2026-09-30T10:00:10Z"), 5, 1);
  assert.equal(await m.flush(new Date("2026-09-30T10:05:00Z")), 0);
  assert.equal(sent.length, 0);
  const on = new Meter({ live: true, url: "http://meter.local", apiKey: "k" }, silentLogger, (async () => new Response("{}")) as typeof fetch);
  on.record("org_abc123", new Date("2026-09-30T10:00:10Z"), 5, 1);
  on.record("org_abc123", new Date("2026-09-30T10:00:50Z"), 1, 0);
  assert.deepEqual(meterEvent(on.minutes.get("org_abc123|2026-09-30T10:00Z")!), {
    transaction_id: "scribase:scribase.analytics.events:org_abc123:2026-09-30T10:00Z:pa",
    external_customer_id: "org_abc123",
    external_subscription_id: "scribase-portfolio-org_abc123",
    code: "scribase.analytics.events",
    timestamp: 1790762400,
    properties: { events: 7, stored: 6, capped: 1 },
  });
  assert.equal(await on.flush(new Date("2026-09-30T10:00:59Z")), 0, "the current minute is not finished");
  assert.equal(await on.flush(new Date("2026-09-30T10:01:00Z")), 1);
});
