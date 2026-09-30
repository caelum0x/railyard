// The wire protocol: every capture endpoint and every encoding stores the
// right fields; malformed or oversized requests are a plain 400/413 and
// store nothing; an unknown token is a 401.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { randomUUID } from "node:crypto";
import { gzipSync, strToU8 } from "fflate";
import { count, makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "./helpers.ts";

let pg: ScratchPg;
let run: Running;
const site = makeSite();

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site]);
});

const ev = (name: string, extra: Record<string, unknown> = {}) => ({
  event: name,
  uuid: randomUUID(),
  properties: { distinct_id: "u1", token: site.token, $lib: "raw", ...extra },
  timestamp: new Date().toISOString(),
});

const post = (path: string, body: BodyInit, headers: Record<string, string> = {}) => fetch(`${run.base}${path}`, { method: "POST", body, headers });

test("every endpoint and encoding stores the event with its fields", { skip: skipNoPg }, async () => {
  const cases: [string, () => Promise<Response>, string][] = [];
  const add = (label: string, f: (e: ReturnType<typeof ev>) => Promise<Response>) => {
    const e = ev(`ev_${label}`, { $session_id: "s1", $window_id: "w1", $elements_chain: "button.buy:text=\"Buy\"" });
    cases.push([label, () => f(e), e.uuid]);
  };
  add("json_e", (e) => post("/e/", JSON.stringify(e), { "content-type": "application/json" }));
  add("json_i_v0", (e) => post("/i/v0/e/", JSON.stringify([e]), { "content-type": "text/plain" }));
  add("batch", (e) => post("/batch/", JSON.stringify({ api_key: site.token, batch: [{ ...e, distinct_id: "u1" }], sent_at: new Date().toISOString() }), { "content-type": "application/json" }));
  add("capture", (e) => post("/capture/", JSON.stringify(e), { "content-type": "application/json" }));
  add("gzip_js", (e) => post("/e/?compression=gzip-js", gzipSync(strToU8(JSON.stringify([e]))), { "content-type": "text/plain" }));
  add("content_encoding_gzip", (e) => post("/batch/", gzipSync(strToU8(JSON.stringify({ api_key: site.token, batch: [e] }))), { "content-type": "application/json", "content-encoding": "gzip" }));
  add("base64_form", (e) => post("/e/?compression=base64", `data=${encodeURIComponent(Buffer.from(JSON.stringify([e])).toString("base64"))}`, { "content-type": "application/x-www-form-urlencoded" }));
  add("form_json", (e) => post("/e/", `data=${encodeURIComponent(JSON.stringify(e))}`, { "content-type": "application/x-www-form-urlencoded" }));
  add("get_data", (e) => fetch(`${run.base}/e/?data=${encodeURIComponent(Buffer.from(JSON.stringify(e)).toString("base64"))}`));
  for (const [label, send, uuid] of cases) {
    const res = await send();
    assert.equal(res.status, 200, `${label}: ${await res.clone().text()}`);
    assert.deepEqual(await res.json(), { status: 1 }, label);
    await run.svc.deps.writer.flush();
    const row = pg.psql(`select event || '|' || distinct_id || '|' || coalesce(session_id,'') || '|' || coalesce(window_id,'') || '|' || coalesce(elements_chain,'') || '|' || (properties ->> '$lib') from pa.events where uuid = '${uuid}' and project_id = '${site.project_id}'`);
    assert.equal(row, `ev_${label}|u1|s1|w1|button.buy:text="Buy"|raw`, label);
  }
});

test("CORS preflight is answered for browsers", { skip: skipNoPg }, async () => {
  const res = await fetch(`${run.base}/e/`, { method: "OPTIONS", headers: { origin: "https://shop.example" } });
  assert.equal(res.status, 204);
  assert.equal(res.headers.get("access-control-allow-origin"), "https://shop.example");
});

test("malformed, oversized and unknown-token requests fail whole and store nothing", { skip: skipNoPg }, async () => {
  const before = count(pg, "true");
  const bad: [string, Promise<Response>, number][] = [
    ["not json", post("/e/", "{nope", { "content-type": "application/json" }), 400],
    ["bad gzip", post("/e/?compression=gzip-js", new Uint8Array([0x1f, 0x8b, 1, 2, 3]), {}), 400],
    ["bad base64", post("/e/?compression=base64", "data=%%%%", { "content-type": "application/x-www-form-urlencoded" }), 400],
    ["one bad event in a batch", post("/batch/", JSON.stringify({ api_key: site.token, batch: [{ ...ev("ok"), distinct_id: "u1" }, { event: "", distinct_id: "u1" }] }), {}), 400],
    ["missing distinct id", post("/e/", JSON.stringify({ event: "x", properties: { token: site.token } }), {}), 400],
    ["bad uuid", post("/e/", JSON.stringify({ ...ev("x"), uuid: "not-a-uuid" }), {}), 400],
    ["mixed tokens", post("/batch/", JSON.stringify({ api_key: site.token, batch: [ev("a"), { ...ev("b"), properties: { distinct_id: "u", token: "site_other000000000" } }] }), {}), 400],
    ["too many events", post("/batch/", JSON.stringify({ api_key: site.token, batch: Array.from({ length: 1001 }, () => ({ ...ev("m"), distinct_id: "u1" })) }), {}), 413],
    ["body too large", post("/e/", "x".repeat(2 * 1024 * 1024 + 10), { "content-type": "application/json" }), 413],
    ["gzip bomb", post("/e/?compression=gzip-js", gzipSync(new Uint8Array(20 * 1024 * 1024)), {}), 413],
    ["unknown token", post("/e/", JSON.stringify({ ...ev("x"), properties: { distinct_id: "u", token: "site_unknown000000" } }), {}), 401],
    ["no token", post("/e/", JSON.stringify({ event: "x", distinct_id: "u" }), {}), 401],
    ["empty batch", post("/batch/", JSON.stringify({ api_key: site.token, batch: [] }), {}), 400],
  ];
  for (const [label, p, status] of bad) {
    const res = await p;
    assert.equal(res.status, status, label);
    const body = (await res.json()) as { type: string; detail: string };
    assert.ok(body.detail.length > 0, label);
  }
  await run.svc.deps.writer.flush();
  assert.equal(count(pg, "true"), before);
});

test("remote config answers per token; unknown tokens are 404", { skip: skipNoPg }, async () => {
  const res = await fetch(`${run.base}/array/${site.token}/config`);
  assert.equal(res.status, 200);
  const cfg = (await res.json()) as Record<string, unknown>;
  assert.equal(cfg.token, site.token);
  assert.equal(cfg.sessionRecording, false);
  assert.deepEqual(cfg.supportedCompression, ["gzip", "gzip-js"]);
  const js = await fetch(`${run.base}/array/${site.token}/config.js`);
  assert.equal(js.status, 200);
  assert.match(js.headers.get("content-type") ?? "", /javascript/);
  assert.match(await js.text(), new RegExp(site.token));
  assert.equal((await fetch(`${run.base}/array/site_unknown000000/config`)).status, 404);
});

test("an event over 1 MB is dropped and counted; the rest of its batch is stored", { skip: skipNoPg }, async () => {
  // A 2 MB gzip body can decode to 16 MB; one such event would otherwise be
  // stored whole and later read into the service by exports and results.
  const big = { ...ev("huge", { blob: "x".repeat(1024 * 1024 + 1) }), distinct_id: "u-big" };
  const small = { ...ev("fits"), distinct_id: "u-big" };
  const res = await post("/batch/?compression=gzip-js", gzipSync(strToU8(JSON.stringify({ api_key: site.token, batch: [big, small] }))), {});
  assert.equal(res.status, 200, "a normal answer so the SDK does not retry the batch forever");
  await run.svc.deps.writer.flush();
  assert.equal(count(pg, `distinct_id = 'u-big' and event = 'huge'`), 0);
  assert.equal(count(pg, `distinct_id = 'u-big' and event = 'fits'`), 1);
  assert.ok(Number(pg.psql(`select coalesce(sum(dropped), 0) from pa.usage_monthly where project_id = '${site.project_id}'`)) >= 1, "the drop is counted");
});
