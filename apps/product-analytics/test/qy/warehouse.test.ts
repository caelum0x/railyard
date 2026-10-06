// A22: warehouse destinations. A project's events land as rows in the
// customer's own ClickHouse table (created when missing, a retried batch
// collapsing by uuid) or BigQuery table (service-account sign-in, insertId per
// event, the table created once on 404). Secrets (the ClickHouse password, the
// service account key) are never read back, and another project's rows never
// leave.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { createVerify, generateKeyPairSync, randomBytes } from "node:crypto";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { CURSOR_LAG_MS, runExports } from "../../src/exports/runner.ts";
import { checkWarehouse, serviceAccountAssertion } from "../../src/exports/warehouse.ts";
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
let bqTables = new Set<string>();
const insertIds = new Set<string>();
const site = makeSite({ name: "shop" });
const other = makeSite({ name: "other" });
let now = new Date(Math.ceil(Date.now() / 3600_000) * 3600_000);
const later = () => (now = new Date(now.getTime() + CURSOR_LAG_MS + 3600_000));
const { privateKey, publicKey } = generateKeyPairSync("rsa", { modulusLength: 2048 });
const privatePem = privateKey.export({ type: "pkcs8", format: "pem" }).toString();

const serviceAccount = () =>
  JSON.stringify({ type: "service_account", client_email: "export@shop-analytics.iam.gserviceaccount.com", private_key: privatePem, token_uri: `${base}/token` });

before(async () => {
  if (skipNoPg) return;
  const srv = createServer((req, res) => {
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", () => {
      const path = req.url ?? "";
      got.push({ path, headers: Object.fromEntries(Object.entries(req.headers).map(([k, v]) => [k, String(v)])), body });
      if (path.startsWith("/token")) {
        // Verify the RS256 assertion like Google would.
        const assertion = new URLSearchParams(body).get("assertion") ?? "";
        const [h, c, s] = assertion.split(".");
        const ok = createVerify("RSA-SHA256").update(`${h}.${c}`).verify(publicKey, Buffer.from(s, "base64url"));
        return void res.writeHead(ok ? 200 : 401, { "content-type": "application/json" }).end(JSON.stringify(ok ? { access_token: "ya29.test", expires_in: 3600 } : { error: "invalid_grant" }));
      }
      if (path.startsWith("/bq/")) {
        if (req.headers.authorization !== "Bearer ya29.test") return void res.writeHead(401).end("{}");
        const insert = path.match(/\/tables\/([^/]+)\/insertAll$/);
        if (insert) {
          if (!bqTables.has(insert[1])) return void res.writeHead(404).end(JSON.stringify({ error: { message: "Not found: Table" } }));
          for (const r of JSON.parse(body).rows) insertIds.add(r.insertId);
          return void res.writeHead(200, { "content-type": "application/json" }).end(JSON.stringify({ kind: "bigquery#tableDataInsertAllResponse" }));
        }
        if (path.endsWith("/tables")) {
          bqTables.add(JSON.parse(body).tableReference.tableId);
          return void res.writeHead(200).end("{}");
        }
      }
      // ClickHouse HTTP interface.
      if (path.startsWith("/ch") && req.headers["x-clickhouse-key"] !== "ch-pass") return void res.writeHead(516).end("Code: 516. Authentication failed");
      res.writeHead(200).end("");
    });
  });
  await new Promise<void>((r) => srv.listen(0, "127.0.0.1", () => r()));
  base = `http://127.0.0.1:${(srv.address() as AddressInfo).port}`;
  process.env.PA_BIGQUERY_URL = `${base}/bq`;
  pg = await migratedPg();
  run = await startService(pg, [site, other], { consoleSecret: SECRET, now: () => now });
});

const ev = (n: number, prefix: string, at: Date): Row[] =>
  Array.from({ length: n }, (_, i) => ({ event: "purchase", distinct_id: `${prefix}${i}`, timestamp: at, properties: { amount: i + 1, $current_url: `https://shop.example/p${i}` } }));

test("inputs: identifiers and keys are checked, secrets stay sealed", () => {
  assert.throws(() => checkWarehouse("clickhouse", { url: "https://ch.example", table: "events; drop" }, "x"), /letters, digits/);
  assert.throws(() => checkWarehouse("bigquery", { project_id: "Bad Project", dataset: "d" }, "{}"), /project id/);
  assert.throws(() => checkWarehouse("bigquery", { project_id: "shop-analytics", dataset: "d" }, "not json"), /JSON key/);
  const saved = process.env.PA_EXPORTS_ALLOW_PRIVATE;
  delete process.env.PA_EXPORTS_ALLOW_PRIVATE;
  try {
    // Outside tests the signed assertion only goes to Google.
    assert.throws(
      () => checkWarehouse("bigquery", { project_id: "shop-analytics", dataset: "d" }, JSON.stringify({ client_email: "a@b.c", private_key: "-----BEGIN PRIVATE KEY-----x", token_uri: "https://evil.example/token" })),
      /token_uri/,
    );
    assert.throws(() => checkWarehouse("clickhouse", { url: "http://ch.example" }, "x"), /https/);
  } finally {
    process.env.PA_EXPORTS_ALLOW_PRIVATE = saved;
  }
  const ok = checkWarehouse("clickhouse", { url: "https://ch.example:8443/", user: "writer" }, "pw");
  assert.deepEqual(ok.config, { url: "https://ch.example:8443", database: "default", table: "railyard_events", user: "writer", label: "ClickHouse" });
  assert.ok(!JSON.stringify(ok.config).includes("pw"));
  // The assertion is a three-part RS256 JWT.
  assert.equal(serviceAccountAssertion({ client_email: "a@b.c", private_key: privatePem, token_uri: "https://oauth2.googleapis.com/token" }, new Date()).split(".").length, 3);
});

test("ClickHouse gets the project's rows in its own table, never the other project's", { skip: skipNoPg }, async () => {
  const made = await api(run, site, "/exports", { now, body: { name: "ch", kind: "clickhouse", secret: "ch-pass", start: "all", config: { url: `${base}/ch`, database: "analytics", table: "shop_events", user: "writer" } } });
  assert.equal(made.status, 201, JSON.stringify(made.body));
  const listed = JSON.stringify((await api(run, site, "/exports", { now })).body);
  assert.ok(!listed.includes("ch-pass"), "the password is never read back");
  await insertEvents(pg, site.project_id, ev(3, "c", new Date(now.getTime() - 1000)));
  await insertEvents(pg, other.project_id, ev(2, "leak", new Date(now.getTime() - 1000)));
  got.length = 0;
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  const ch = got.filter((g) => g.path.startsWith("/ch"));
  const queries = ch.map((g) => decodeURIComponent(new URL(`http://x${g.path}`).searchParams.get("query") ?? ""));
  assert.match(queries[0], /^CREATE TABLE IF NOT EXISTS `analytics`.`shop_events` .*ReplacingMergeTree ORDER BY \(event, uuid\)/);
  assert.match(queries[1], /^INSERT INTO `analytics`.`shop_events` FORMAT JSONEachRow$/);
  const rows = ch[1].body.split("\n").map((l) => JSON.parse(l));
  assert.equal(rows.length, 3);
  assert.deepEqual(rows.map((r) => r.distinct_id).sort(), ["c0", "c1", "c2"]);
  assert.match(rows[0].timestamp, /^\d{4}-\d\d-\d\d \d\d:\d\d:\d\d/);
  assert.equal(JSON.parse(rows[0].properties).$current_url.startsWith("https://shop.example/"), true);
  assert.equal(ch[1].headers["x-clickhouse-user"], "writer");
  assert.ok(!got.some((g) => g.body.includes("leak")), "no other project's rows");
  const d = (await api(run, site, "/exports", { now })).body.destinations.find((x: { kind: string }) => x.kind === "clickhouse");
  assert.equal(d.status, "ok");
});

test("BigQuery signs in with the service account, creates the table once and dedupes by insertId", { skip: skipNoPg }, async () => {
  // Pause the ClickHouse destination so only BigQuery sends.
  for (const d of (await api(run, site, "/exports", { now })).body.destinations) await api(run, site, `/exports/${d.id}`, { now, method: "PUT", body: { enabled: false } });
  bqTables = new Set();
  const made = await api(run, site, "/exports", { now, body: { name: "bq", kind: "bigquery", secret: serviceAccount(), start: "all", config: { project_id: "shop-analytics", dataset: "events" } } });
  assert.equal(made.status, 201, JSON.stringify(made.body));
  assert.ok(!JSON.stringify((await api(run, site, "/exports", { now })).body).includes("PRIVATE KEY"), "the key is never read back");
  await insertEvents(pg, site.project_id, ev(4, "b", new Date(now.getTime() + 1000)));
  got.length = 0;
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  const paths = got.map((g) => g.path);
  assert.deepEqual(paths, [
    "/token",
    "/bq/bigquery/v2/projects/shop-analytics/datasets/events/tables/railyard_events/insertAll",
    "/bq/bigquery/v2/projects/shop-analytics/datasets/events/tables",
    "/bq/bigquery/v2/projects/shop-analytics/datasets/events/tables/railyard_events/insertAll",
  ]);
  // Every kept event of this project (3 from the ClickHouse test, 4 now), one row each.
  assert.equal(insertIds.size, 7, "one row per event, keyed by the event uuid");
  const created = JSON.parse(got[2].body);
  assert.equal(created.schema.fields.find((f: { name: string }) => f.name === "properties").type, "JSON");
  // The next batch goes straight in (the table exists).
  await insertEvents(pg, site.project_id, ev(1, "b2", new Date(now.getTime() + 1000)));
  got.length = 0;
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  assert.deepEqual(got.map((g) => g.path.split("/").pop()), ["token", "insertAll"]);
  assert.equal(insertIds.size, 8);
});

test("a wrong ClickHouse password is a failed, retried delivery with the warehouse's reason", { skip: skipNoPg }, async () => {
  for (const d of (await api(run, site, "/exports", { now })).body.destinations) await api(run, site, `/exports/${d.id}`, { now, method: "PUT", body: { enabled: false } });
  const made = await api(run, site, "/exports", { now, body: { name: "ch-bad", kind: "clickhouse", secret: "wrong", start: "all", config: { url: `${base}/ch` } } });
  await insertEvents(pg, site.project_id, ev(1, "x", new Date(now.getTime() + 1000)));
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  const d = (await api(run, site, `/exports/${made.body.destination.id}`, { now })).body.destination;
  assert.equal(d.status, "retrying");
  assert.match(d.last_error, /ClickHouse answered 516: Code: 516\. Authentication failed/);
});
