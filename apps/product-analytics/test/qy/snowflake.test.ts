// A22: the Snowflake warehouse destination. Key-pair sign-in (RS256 JWT whose
// issuer carries the public key fingerprint), the table created when missing,
// one array-bound INSERT per batch under a requestId derived from the batch
// (so Snowflake does not run a retried batch twice), 202 polling, errors.
// No database needed: a local fake speaks the SQL API.
import { after, before, test } from "node:test";
import assert from "node:assert/strict";
import { createHash, createPublicKey, createVerify, generateKeyPairSync } from "node:crypto";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { ApiError } from "../../src/replay/shared/console-guard.ts";
import { DeliveryError, type ExportedEvent } from "../../src/exports/runner.ts";
import { batchRequestId, checkWarehouse, describeWarehouse, sendWarehouse, snowflakeJwt } from "../../src/exports/warehouse.ts";

const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const pem = privateKey.export({ type: "pkcs8", format: "pem" }).toString();
const fingerprint = `SHA256:${createHash("sha256").update(createPublicKey(privateKey).export({ type: "spki", format: "der" })).digest("base64")}`;

interface Got {
  method: string;
  path: string;
  headers: Record<string, string>;
  body: string;
}
const got: Got[] = [];
const seenRequestIds = new Map<string, number>();
let pendingOnce = false;
let failInsert = false;
let server: Server;

before(async () => {
  server = createServer((req, res) => {
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", () => {
      const path = req.url ?? "";
      got.push({ method: req.method ?? "", path, headers: Object.fromEntries(Object.entries(req.headers).map(([k, v]) => [k, String(v)])), body });
      const token = String(req.headers.authorization ?? "").replace(/^Bearer /, "");
      const [h, c, s] = token.split(".");
      const ok = !!s && createVerify("RSA-SHA256").update(`${h}.${c}`).verify(publicKey, Buffer.from(s, "base64url"));
      const claims = ok ? JSON.parse(Buffer.from(c, "base64url").toString()) : {};
      if (!ok || claims.iss !== `XY12345.LOADER.${fingerprint}` || req.headers["x-snowflake-authorization-token-type"] !== "KEYPAIR_JWT") {
        return void res.writeHead(401, { "content-type": "application/json" }).end(JSON.stringify({ message: "JWT token is invalid." }));
      }
      if (req.method === "GET") return void res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ statementStatusUrl: path, data: [["2"]] }));
      const id = new URL(path, "http://x").searchParams.get("requestId") ?? "";
      seenRequestIds.set(id, (seenRequestIds.get(id) ?? 0) + 1);
      const stmt = JSON.parse(body).statement as string;
      if (stmt.startsWith("INSERT") && failInsert) return void res.writeHead(422, { "content-type": "application/json" }).end(JSON.stringify({ message: "SQL compilation error: invalid identifier" }));
      if (stmt.startsWith("INSERT") && pendingOnce) {
        pendingOnce = false;
        return void res.writeHead(202, { "content-type": "application/json" }).end(JSON.stringify({ statementHandle: "01b2-0000-abcd", message: "Asynchronous execution in progress." }));
      }
      res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ statementHandle: "01b2-0000-done", data: [["1"]] }));
    });
  });
  await new Promise<void>((r) => server.listen(0, "127.0.0.1", () => r()));
  process.env.PA_SNOWFLAKE_URL = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
  process.env.PA_SNOWFLAKE_POLL_MS = "5";
});
after(() => server.close());

const config = { account: "xy12345.eu-west-1", user: "loader", warehouse: "LOAD_WH", database: "ANALYTICS", schema: "PUBLIC", table: "EVENTS", role: "LOADER_ROLE" };
const ev = (n: number): ExportedEvent => ({
  uuid: `0190a6f0-0000-7000-8000-00000000000${n}`,
  event: n === 1 ? "signed_up" : "$pageview",
  distinct_id: `user-${n}`,
  person_id: `p-${n}`,
  timestamp: "2026-10-06T10:00:00.000Z",
  properties: { n },
  elements_chain: null,
  session_id: null,
});

test("checkWarehouse validates a Snowflake destination and keeps the key sealed", () => {
  const { config: kept, target } = checkWarehouse("snowflake", { ...config }, pem);
  assert.equal(kept.account, "xy12345.eu-west-1");
  assert.equal(kept.schema, "PUBLIC");
  assert.equal(target, pem.trim());
  assert.ok(!JSON.stringify(kept).includes("PRIVATE KEY"));
  assert.equal(describeWarehouse("snowflake", kept), "Snowflake: ANALYTICS.PUBLIC.EVENTS");
  assert.throws(() => checkWarehouse("snowflake", { ...config, table: "events; drop" }, pem), ApiError);
  assert.throws(() => checkWarehouse("snowflake", { ...config, account: "bad/acct" }, pem), ApiError);
  assert.throws(() => checkWarehouse("snowflake", { ...config }, "not a key"), ApiError);
  assert.throws(() => checkWarehouse("snowflake", { ...config, user: undefined }, pem), ApiError);
});

test("the key-pair JWT carries the account, user and public key fingerprint", () => {
  const jwt = snowflakeJwt("xy12345.eu-west-1", "loader", pem, new Date("2026-10-06T10:00:00Z"));
  const claims = JSON.parse(Buffer.from(jwt.split(".")[1], "base64url").toString());
  assert.equal(claims.iss, `XY12345.LOADER.${fingerprint}`);
  assert.equal(claims.sub, "XY12345.LOADER");
  assert.equal(claims.exp - claims.iat, 59 * 60);
});

test("a batch creates the table, then inserts every row with one array-bound statement", async () => {
  got.length = 0;
  const events = [ev(1), ev(2)];
  const r = await sendWarehouse("snowflake", config, pem, events, new Date(), fetch);
  assert.equal(r.status, 200);
  assert.equal(got.length, 2);
  const ddl = JSON.parse(got[0].body);
  assert.match(ddl.statement, /^CREATE TABLE IF NOT EXISTS "ANALYTICS"\."PUBLIC"\."EVENTS"/);
  assert.equal(ddl.warehouse, "LOAD_WH");
  assert.equal(ddl.role, "LOADER_ROLE");
  const insert = JSON.parse(got[1].body);
  assert.match(insert.statement, /^INSERT INTO "ANALYTICS"\."PUBLIC"\."EVENTS" \("UUID", "EVENT"/);
  assert.deepEqual(insert.bindings["1"].value, events.map((e) => e.uuid));
  assert.deepEqual(insert.bindings["6"].value, ['{"n":1}', '{"n":2}']);
  assert.match(got[1].path, new RegExp(`requestId=${batchRequestId(events)}&retry=true`));
});

test("a retried batch reuses its requestId; another batch does not", async () => {
  assert.equal(batchRequestId([ev(1), ev(2)]), batchRequestId([ev(1), ev(2)]));
  assert.notEqual(batchRequestId([ev(1)]), batchRequestId([ev(2)]));
  assert.match(batchRequestId([ev(3)]), /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-8[0-9a-f]{3}-[0-9a-f]{12}$/);
});

test("a still-running insert is polled until it settles", async () => {
  got.length = 0;
  pendingOnce = true;
  const r = await sendWarehouse("snowflake", config, pem, [ev(3)], new Date(), fetch);
  assert.equal(r.status, 200);
  assert.equal(got.at(-1)?.method, "GET");
  assert.match(got.at(-1)?.path ?? "", /\/api\/v2\/statements\/01b2-0000-abcd$/);
});

test("Snowflake errors become delivery errors with a short excerpt", async () => {
  failInsert = true;
  await assert.rejects(() => sendWarehouse("snowflake", config, pem, [ev(4)], new Date(), fetch), (e: unknown) => e instanceof DeliveryError && /Snowflake answered 422/.test(e.message));
  failInsert = false;
  const wrongUser = { ...config, user: "intruder" };
  await assert.rejects(() => sendWarehouse("snowflake", wrongUser, pem, [ev(5)], new Date(), fetch), (e: unknown) => e instanceof DeliveryError && /401/.test(e.message));
  assert.deepEqual(await sendWarehouse("snowflake", config, pem, [], new Date(), fetch), { bytes: 0, status: null });
});
