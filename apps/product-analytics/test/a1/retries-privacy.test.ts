// A retried batch (same event uuids) is stored once, also when the retry
// lands in the same insert buffer. No IP address is in any table or log:
// a grep over a full data dump and the captured log lines of a run.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { jsonLogger } from "../../src/log.ts";
import { count, makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "./helpers.ts";

let pg: ScratchPg;
let run: Running;
const logs: string[] = [];
const site = makeSite();
const IPS = ["203.0.113.77", "198.51.100.23", "2001:db8::77"];

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site], { log: jsonLogger((l) => logs.push(l)) });
});

const body = (uuids: string[]) => JSON.stringify({ api_key: site.token, batch: uuids.map((uuid, i) => ({ event: `r${i}`, distinct_id: "retry-user", uuid })) });
const send = (b: string, headers: Record<string, string> = {}) => fetch(`${run.base}/batch/`, { method: "POST", body: b, headers: { "content-type": "application/json", ...headers } });

test("a retried batch is stored once, across flushes and within one buffer", { skip: skipNoPg }, async () => {
  const uuids = Array.from({ length: 20 }, () => randomUUID());
  assert.equal((await send(body(uuids))).status, 200);
  assert.equal((await send(body(uuids))).status, 200);
  await run.svc.deps.writer.flush();
  assert.equal((await send(body(uuids))).status, 200);
  await run.svc.deps.writer.flush();
  assert.equal(count(pg, `project_id = '${site.project_id}' and distinct_id = 'retry-user'`), 20);
  assert.equal(pg.psql(`select stored from pa.usage_monthly where project_id = '${site.project_id}'`), "20");
});

test("no IP address is stored or logged; country comes from CF-IPCountry", { skip: skipNoPg }, async () => {
  const uuid = randomUUID();
  const res = await fetch(`${run.base}/e/`, {
    method: "POST",
    headers: { "x-forwarded-for": `${IPS[0]}, ${IPS[1]}`, "cf-connecting-ip": IPS[0], "x-real-ip": IPS[1], "true-client-ip": IPS[2], "cf-ipcountry": "DE" },
    body: JSON.stringify({ event: "ip_check", uuid, properties: { distinct_id: "ip-user", token: site.token, $ip: IPS[0], ip: IPS[1], $raw_ip: IPS[2], $set: { $ip: IPS[0] } } }),
  });
  assert.equal(res.status, 200);
  // An error path too: a bad request is logged without its headers.
  await fetch(`${run.base}/e/`, { method: "POST", headers: { "x-forwarded-for": IPS[0] }, body: "{bad" });
  await run.svc.deps.writer.flush();
  assert.equal(pg.psql(`select properties ->> '$geoip_country_code' from pa.events where uuid = '${uuid}'`), "DE");
  assert.equal(pg.psql(`select count(*) from pa.events where uuid = '${uuid}' and properties ? '$ip'`), "0");
  const dump = execFileSync("pg_dump", ["-h", pg.dir, "-p", new URL(pg.url).searchParams.get("port")!, "-U", "postgres", "--data-only", "--schema=pa", "postgres"], { encoding: "utf8", maxBuffer: 64 * 1024 * 1024 });
  assert.ok(dump.includes("ip_check"), "the dump holds the run's data");
  const everything = `${dump}\n${logs.join("\n")}`;
  for (const ip of [...IPS, "127.0.0.1", "::1", "::ffff:"]) assert.ok(!everything.includes(ip), `found ${ip}`);
});
