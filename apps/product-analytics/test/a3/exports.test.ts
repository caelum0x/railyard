// Exports: a webhook destination receives Standard Webhooks-signed batches
// in order and a 500 is retried with exponential backoff; an S3 destination
// (a local fake that checks every AWS Signature V4 itself) receives
// NDJSON.gz; a destination never receives another project's rows; secrets
// are sealed at rest and shown once; private addresses are refused; an
// insight's answer downloads as CSV.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { createHash, createHmac, randomBytes } from "node:crypto";
import { createServer, type IncomingMessage } from "node:http";
import type { AddressInfo } from "node:net";
import { gunzipSync } from "fflate";
import { verify } from "../../src/bridge/standard-webhooks.ts";
import { open, seal } from "../../src/exports/secrets.ts";
import { checkReachable, checkUrlShape, isPrivateAddress } from "../../src/exports/net-guard.ts";
import { backoffMs, CURSOR_LAG_MS, runExports } from "../../src/exports/runner.ts";
import { insightTable } from "../../src/exports/csv.ts";
import { silentLogger } from "../../src/log.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { api, insertEvents, SECRET, token } from "./helpers.ts";

process.env.PA_EXPORTS_KEY = randomBytes(32).toString("base64");
process.env.PA_EXPORTS_ALLOW_PRIVATE = "1";

let pg: ScratchPg;
let run: Running;
const site = makeSite({ name: "shop" });
const other = makeSite({ name: "other" });

const body = (req: IncomingMessage) =>
  new Promise<Buffer>((resolve) => {
    const parts: Buffer[] = [];
    req.on("data", (d) => parts.push(d));
    req.on("end", () => resolve(Buffer.concat(parts)));
  });

interface Hook {
  url: string;
  got: { headers: Record<string, string>; body: string }[];
  failNext: number;
}
async function hook(): Promise<Hook> {
  const h: Hook = { url: "", got: [], failNext: 0 };
  const srv = createServer(async (req, res) => {
    const b = (await body(req)).toString("utf8");
    if (h.failNext > 0) {
      h.failNext -= 1;
      return void res.writeHead(500).end("busy");
    }
    h.got.push({ headers: req.headers as Record<string, string>, body: b });
    res.writeHead(204).end();
  });
  await new Promise<void>((r) => srv.listen(0, "127.0.0.1", () => r()));
  srv.unref();
  h.url = `http://127.0.0.1:${(srv.address() as AddressInfo).port}/hooks/analytics`;
  return h;
}

// A local S3-compatible fake that verifies AWS Signature V4 on every request with its own code.
const S3_KEY = { id: "AKIDEXAMPLE000000001", secret: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY" };
const hmac = (k: Buffer | string, s: string) => createHmac("sha256", k).update(s).digest();
const sha = (b: Buffer | string) => createHash("sha256").update(b).digest("hex");
function sigV4Ok(req: IncomingMessage, payload: Buffer): string | null {
  const auth = req.headers.authorization ?? "";
  const m = /^AWS4-HMAC-SHA256 Credential=([^/]+)\/(\d{8})\/([^/]+)\/s3\/aws4_request, ?SignedHeaders=([^,]+), ?Signature=([0-9a-f]{64})$/.exec(auth);
  if (!m || m[1] !== S3_KEY.id) return "bad authorization header";
  const [, , day, region, signed, sig] = m;
  const amzDate = String(req.headers["x-amz-date"] ?? "");
  if (!amzDate.startsWith(day)) return "date mismatch";
  if (req.headers["x-amz-content-sha256"] !== sha(payload)) return "payload hash mismatch";
  const url = new URL(req.url ?? "/", "http://x");
  const query = [...url.searchParams.entries()].sort().map(([k, v]) => `${encodeURIComponent(k)}=${encodeURIComponent(v)}`).join("&");
  const names = signed.split(";");
  const canonical = [req.method, url.pathname, query, names.map((n) => `${n}:${String(req.headers[n] ?? "").trim()}\n`).join(""), signed, req.headers["x-amz-content-sha256"]].join("\n");
  const scope = `${day}/${region}/s3/aws4_request`;
  const toSign = ["AWS4-HMAC-SHA256", amzDate, scope, sha(canonical)].join("\n");
  const key = hmac(hmac(hmac(hmac(`AWS4${S3_KEY.secret}`, day), region), "s3"), "aws4_request");
  return hmac(key, toSign).toString("hex") === sig ? null : "signature mismatch";
}
const bucket = new Map<string, Buffer>();
const s3Refusals: string[] = [];
let s3Base = "";

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site, other], { consoleSecret: SECRET });
  const s3 = createServer(async (req, res) => {
    const payload = await body(req);
    const bad = sigV4Ok(req, payload);
    if (bad) {
      s3Refusals.push(bad);
      return void res.writeHead(403).end(`<Error><Code>SignatureDoesNotMatch</Code><Message>${bad}</Message></Error>`);
    }
    if (req.method !== "PUT") return void res.writeHead(405).end();
    bucket.set(decodeURIComponent(new URL(req.url ?? "/", "http://x").pathname), payload);
    res.writeHead(200, { etag: `"${sha(payload).slice(0, 32)}"` }).end();
  });
  await new Promise<void>((r) => s3.listen(0, "127.0.0.1", () => r()));
  s3.unref();
  s3Base = `http://127.0.0.1:${(s3.address() as AddressInfo).port}`;
});

const later = () => new Date(Date.now() + CURSOR_LAG_MS + 5_000);
const events = (prefix: string, n: number, extra: Record<string, unknown> = {}) =>
  Array.from({ length: n }, (_, i) => ({ event: i % 2 ? "purchase" : "$pageview", distinct_id: `${prefix}${i}`, timestamp: new Date(Date.now() - 60_000), properties: { n: i, ...extra } }));

test("secrets are sealed with AES-256-GCM; private addresses and bad URLs are refused", async () => {
  const s = seal("whsec_abc");
  assert.notEqual(s, "whsec_abc");
  assert.equal(open(s), "whsec_abc");
  assert.throws(() => open(s, randomBytes(32)));
  for (const ip of ["127.0.0.1", "10.1.2.3", "172.20.0.1", "192.168.1.1", "169.254.169.254", "100.64.0.1", "::1", "fd00::1", "fe80::1", "::ffff:10.0.0.1", "0.0.0.0"]) assert.equal(isPrivateAddress(ip), true, ip);
  for (const ip of ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"]) assert.equal(isPrivateAddress(ip), false, ip);
  process.env.PA_EXPORTS_ALLOW_PRIVATE = "0";
  try {
    assert.throws(() => checkUrlShape("http://example.com/x", "The URL"), /https/);
    assert.throws(() => checkUrlShape("https://user:pw@example.com/x", "The URL"), /user name/);
    await assert.rejects(checkReachable(new URL("https://127.0.0.1/x")), /not a public address/);
    await assert.rejects(checkReachable(new URL("https://[::1]/x")), /not a public address/);
  } finally {
    process.env.PA_EXPORTS_ALLOW_PRIVATE = "1";
  }
  assert.deepEqual([1, 2, 3, 7, 8, 20].map(backoffMs), [30_000, 60_000, 120_000, 1_920_000, 3_600_000, 3_600_000], "30 s doubling, at most an hour");
});

test("webhook: signed batches of this project's events only, in order, once each; the secret shows once", { skip: skipNoPg }, async () => {
  const mine = await hook();
  const theirs = await hook();
  assert.equal((await api(run, site, "/exports", { body: { name: "hook", kind: "webhook", config: { url: mine.url } }, role: "viewer" })).status, 403);
  const made = await api(run, site, "/exports", { body: { name: "hook", kind: "webhook", config: { url: mine.url } } });
  assert.equal(made.status, 201, JSON.stringify(made.body));
  const secret = made.body.signing_secret as string;
  assert.match(secret, /^whsec_/);
  const dest = made.body.destination;
  assert.equal(JSON.stringify((await api(run, site, `/exports/${dest.id}`)).body).includes(secret), false, "never read back");
  assert.equal(pg.psql(`select count(*) from pa.export_destinations where secret_enc like '%${secret.slice(6, 20)}%'`), "0", "sealed at rest");
  await api(run, other, "/exports", { body: { name: "hook", kind: "webhook", config: { url: theirs.url } } });

  const ping = await api(run, site, `/exports/${dest.id}/test`, { body: {} });
  assert.deepEqual(ping.body, { ok: true, status: 204 });
  mine.got.length = 0;

  const a = await insertEvents(pg, site.project_id, events("a", 7));
  const b = await insertEvents(pg, other.project_id, events("b", 4));
  await runExports(run.svc.deps.db, later(), { log: silentLogger, batchSize: 3 });
  assert.equal(mine.got.length, 3, "7 events in batches of 3");
  const sent = mine.got.flatMap((g) => {
    const h = g.headers;
    assert.ok(verify(secret, { id: h["webhook-id"], timestamp: h["webhook-timestamp"], signature: h["webhook-signature"] }, g.body, Math.floor(Date.now() / 1000) + CURSOR_LAG_MS / 1000 + 5), "signature verifies");
    const p = JSON.parse(g.body);
    assert.equal(p.type, "events");
    assert.equal(p.project_id, site.project_id);
    return p.events as { uuid: string; event: string; properties: { n: number } }[];
  });
  assert.deepEqual(new Set(sent.map((e) => e.uuid)), new Set(a));
  assert.equal(sent.length, 7, "each once");
  assert.equal(theirs.got.flatMap((g) => JSON.parse(g.body).events).length, 4);
  assert.ok(theirs.got.flatMap((g) => JSON.parse(g.body).events).every((e: { uuid: string }) => b.includes(e.uuid)));

  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  assert.equal(mine.got.length, 3, "nothing new, nothing sent");
  await insertEvents(pg, site.project_id, events("c", 2));
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  assert.equal(mine.got.length, 4);
  const d = (await api(run, site, `/exports/${dest.id}`)).body.destination;
  assert.equal(d.delivered, 9);
  assert.equal(d.status, "ok");
});

test("a 500 is retried with backoff and the batch is delivered once it is accepted", { skip: skipNoPg }, async () => {
  const h = await hook();
  const site2 = makeSite({ name: "retry" });
  run.sites.sites.push(site2);
  const dest = (await api(run, site2, "/exports", { body: { name: "flaky", kind: "webhook", config: { url: h.url }, events: ["purchase"] } })).body.destination;
  const ids = await insertEvents(pg, site2.project_id, events("r", 6));
  h.failNext = 2;
  const t0 = later();
  await runExports(run.svc.deps.db, t0, { log: silentLogger });
  let row = pg.psql(`select attempts || ',' || status || ',' || extract(epoch from next_attempt_at - '${t0.toISOString()}'::timestamptz)::int from pa.export_destinations where id = '${dest.id}'`);
  assert.equal(row, "1,retrying,30");
  await runExports(run.svc.deps.db, new Date(t0.getTime() + 10_000), { log: silentLogger });
  assert.equal(pg.psql(`select attempts from pa.export_destinations where id = '${dest.id}'`), "1", "not due yet: no request");
  await runExports(run.svc.deps.db, new Date(t0.getTime() + 31_000), { log: silentLogger });
  row = pg.psql(`select attempts || ',' || extract(epoch from next_attempt_at - '${new Date(t0.getTime() + 31_000).toISOString()}'::timestamptz)::int from pa.export_destinations where id = '${dest.id}'`);
  assert.equal(row, "2,60");
  await runExports(run.svc.deps.db, new Date(t0.getTime() + 92_000), { log: silentLogger });
  assert.equal(h.got.length, 1);
  const got = JSON.parse(h.got[0].body).events as { uuid: string; event: string }[];
  assert.deepEqual(got.map((e) => e.event), ["purchase", "purchase", "purchase"], "only the named events");
  assert.ok(got.every((e) => ids.includes(e.uuid)));
  const runs = (await api(run, site2, `/exports/${dest.id}/runs`)).body.runs as { status: string; http_status: number | null }[];
  assert.deepEqual(runs.map((r) => `${r.status}:${r.http_status}`).reverse(), ["failed:500", "failed:500", "ok:204"]);
  assert.equal(pg.psql(`select attempts || ',' || status from pa.export_destinations where id = '${dest.id}'`), "0,ok");
});

test("S3: NDJSON.gz objects under the prefix, signed with AWS Signature V4, this project's rows only", { skip: skipNoPg }, async () => {
  const site3 = makeSite({ name: "lake" });
  run.sites.sites.push(site3);
  const made = await api(run, site3, "/exports", { body: { name: "lake", kind: "s3", config: { endpoint: s3Base, bucket: "analytics-lake", region: "eu-central-1", prefix: "railyard/raw", access_key_id: S3_KEY.id }, secret: S3_KEY.secret } });
  assert.equal(made.status, 201, JSON.stringify(made.body));
  assert.equal(made.body.signing_secret, null);
  assert.equal(JSON.stringify(made.body).includes(S3_KEY.secret), false);
  const ids = await insertEvents(pg, site3.project_id, events("s", 5, { plan: "pro" }));
  await insertEvents(pg, other.project_id, events("x", 3));
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  assert.deepEqual(s3Refusals, []);
  const keys = [...bucket.keys()].filter((k) => k.includes(site3.project_id));
  assert.equal(keys.length, 1);
  assert.match(keys[0], new RegExp(`^/analytics-lake/railyard/raw/events/${site3.project_id}/\\d{4}/\\d{2}/\\d{2}/\\d+-[0-9a-f-]{36}\\.ndjson\\.gz$`));
  const lines = new TextDecoder().decode(gunzipSync(bucket.get(keys[0])!)).trim().split("\n").map((l) => JSON.parse(l));
  assert.deepEqual(lines.map((l) => l.uuid).sort(), [...ids].sort());
  assert.equal(lines[0].properties.plan, "pro");
  assert.ok([...bucket.keys()].every((k) => !k.includes(other.project_id)), "no object for a project without an S3 destination");

  const wrong = makeSite({ name: "wrong-key" });
  run.sites.sites.push(wrong);
  const bad = (await api(run, wrong, "/exports", { body: { name: "lake", kind: "s3", config: { endpoint: s3Base, bucket: "analytics-lake", access_key_id: S3_KEY.id }, secret: "not-the-secret" } })).body.destination;
  await insertEvents(pg, wrong.project_id, events("w", 1));
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  assert.match(pg.psql(`select last_error from pa.export_destinations where id = '${bad.id}'`), /403/);
  assert.ok(s3Refusals.includes("signature mismatch"), "the fake really checks signatures");
});

test("destinations: at most three, paused ones wait, and the API never serves another project's", { skip: skipNoPg }, async () => {
  const s = makeSite({ name: "many" });
  run.sites.sites.push(s);
  const h = await hook();
  const ids: string[] = [];
  for (let i = 0; i < 3; i += 1) ids.push((await api(run, s, "/exports", { body: { name: `h${i}`, kind: "webhook", config: { url: h.url } } })).body.destination.id);
  assert.equal((await api(run, s, "/exports", { body: { name: "h3", kind: "webhook", config: { url: h.url } } })).status, 409);
  assert.equal((await api(run, s, "/exports", { body: { name: "bad", kind: "ftp" } })).status, 400);
  assert.equal((await api(run, site, `/exports/${ids[0]}`)).status, 404);
  assert.equal((await api(run, s, `/exports/${ids[0]}`, { method: "PUT", body: { enabled: false } })).body.destination.enabled, false);
  await api(run, s, `/exports/${ids[1]}`, { method: "DELETE" });
  await api(run, s, `/exports/${ids[2]}`, { method: "DELETE" });
  await insertEvents(pg, s.project_id, events("p", 2));
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  assert.equal(h.got.length, 0, "a paused destination receives nothing");
  await api(run, s, `/exports/${ids[0]}`, { method: "PUT", body: { enabled: true, retry_now: true } });
  await runExports(run.svc.deps.db, later(), { log: silentLogger });
  assert.equal(h.got.length, 1);
});

test("CSV: an insight's answer and a saved insight download as CSV", { skip: skipNoPg }, async () => {
  const s = makeSite({ name: "csv" });
  run.sites.sites.push(s);
  const day = (d: number) => new Date(Date.now() - d * 86_400_000);
  await insertEvents(pg, s.project_id, [
    { event: "signup", distinct_id: "u1", timestamp: day(2) },
    { event: "signup", distinct_id: "u2", timestamp: day(2) },
    { event: "signup", distinct_id: "=cmd", timestamp: day(1) },
  ]);
  const query = { kind: "trends", series: [{ event: "signup", math: "total" }], date_range: { date_from: "-7d" }, interval: "day" };
  const res = await fetch(`${run.base}/v1/projects/${s.project_id}/exports/insight.csv`, { method: "POST", headers: { authorization: `Bearer ${await token(s)}` }, body: JSON.stringify({ query }) });
  assert.equal(res.status, 200, await res.clone().text());
  assert.match(res.headers.get("content-type") ?? "", /text\/csv/);
  const lines = (await res.text()).trim().split("\r\n");
  assert.match(lines[0], /^series,breakdown,.*,total$/);
  assert.match(lines[1], /,3$/, "total 3");
  const saved = await api(run, s, "/insights/saved", { body: { name: "Signups!", query } });
  assert.equal(saved.status, 201, JSON.stringify(saved.body));
  const dl = await fetch(`${run.base}/v1/projects/${s.project_id}/exports/saved/${saved.body.insight.id}.csv`, { headers: { authorization: `Bearer ${await token(s)}` } });
  assert.equal(dl.status, 200);
  assert.match(dl.headers.get("content-disposition") ?? "", /filename="Signups\.csv"/);
  assert.equal((await fetch(`${run.base}/v1/projects/${site.project_id}/exports/saved/${saved.body.insight.id}.csv`, { headers: { authorization: `Bearer ${await token(site)}` } })).status, 404);
  const funnel = insightTable({ kind: "funnels", groups: [{ breakdown: null, breakdown_value: null, steps: [{ step: 1, name: "=x", people: 3, from_first: 1, from_previous: 1, dropped: 0, avg_seconds: null, median_seconds: null }] }] });
  assert.deepEqual(funnel.rows[0].slice(0, 4), ["", 1, "=x", 3]);
  const bad = await fetch(`${run.base}/v1/projects/${s.project_id}/exports/insight.csv`, { method: "POST", headers: { authorization: `Bearer ${await token(s)}` }, body: JSON.stringify({ query: { kind: "nope" } }) });
  assert.equal(bad.status, 400);
});
