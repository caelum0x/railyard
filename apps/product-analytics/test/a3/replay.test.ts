// Recordings: the recorder's payload (partly compressed rrweb events, sent
// gzip) is stored as gzip NDJSON chunks in object storage with an index row;
// every typed value is masked in the stored bytes even when the page did not
// mask it; a recording opens from a person, an event or a list of people
// (funnel drop-offs); the player link is signed for 10 minutes; recordings
// expire after the plan's days and are removed by a person's deletion.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { gunzipSync, gzipSync, strFromU8, strToU8 } from "fflate";
import { MemoryObjectStore, useReplayStore } from "../../src/replay/object-store.ts";
import { decompressEvent, extractSnapshots, maskEvent, statsOf, type RrwebEvent } from "../../src/replay/snapshot.ts";
import { expireReplays } from "../../src/replay/query.ts";
import { MAX_RECORDING_MS, REPLAY_RETENTION_DAYS } from "../../src/replay/limits.ts";
import { runDueJobs } from "../../src/gdpr/jobs.ts";
import { silentLogger } from "../../src/log.ts";
import { makeSite, migratedPg, skipNoPg, startService, type Running, type ScratchPg } from "../a1/helpers.ts";
import { api, insertEvents, SECRET } from "./helpers.ts";

const SECRET_TEXT = "typed-secret-4417";
const store = new MemoryObjectStore();
let clock = Date.now();
let pg: ScratchPg;
let run: Running;
const site = makeSite({ name: "shop", retention_days: 365 });
const other = makeSite({ name: "other" });

const packed = (v: unknown) => strFromU8(gzipSync(strToU8(JSON.stringify(v))), true);

/** What the recorder sends for one page: meta, a compressed full snapshot, typing and a click. */
function pageEvents(t0: number, opts: { compressed?: boolean } = {}): RrwebEvent[] {
  const doc = {
    type: 0,
    id: 1,
    childNodes: [
      {
        type: 2, tagName: "html", attributes: {}, id: 2,
        childNodes: [{
          type: 2, tagName: "body", attributes: {}, id: 3,
          childNodes: [
            { type: 2, tagName: "input", attributes: { type: "email", value: SECRET_TEXT }, childNodes: [], id: 4 },
            { type: 2, tagName: "textarea", attributes: { value: SECRET_TEXT }, childNodes: [{ type: 3, textContent: SECRET_TEXT, id: 6 }], id: 5 },
            { type: 2, tagName: "input", attributes: { type: "submit", value: "Buy now" }, childNodes: [], id: 7 },
            { type: 2, tagName: "p", attributes: {}, childNodes: [{ type: 3, textContent: "Visible heading", id: 9 }], id: 8 },
          ],
        }],
      },
    ],
  };
  const full = opts.compressed ? { type: 2, timestamp: t0 + 10, data: packed({ node: doc, initialOffset: { top: 0, left: 0 } }), cv: "2024-10" } : { type: 2, timestamp: t0 + 10, data: { node: doc, initialOffset: { top: 0, left: 0 } } };
  const addNode = { type: 2, tagName: "input", attributes: { value: SECRET_TEXT }, childNodes: [], id: 20 };
  const mutation = opts.compressed
    ? { type: 3, timestamp: t0 + 30, cv: "2024-10", data: { source: 0, texts: packed([]), attributes: packed([{ id: 4, attributes: { value: SECRET_TEXT } }]), removes: packed([]), adds: packed([{ parentId: 3, nextId: null, node: addNode }]) } }
    : { type: 3, timestamp: t0 + 30, data: { source: 0, texts: [], attributes: [{ id: 4, attributes: { value: SECRET_TEXT } }], removes: [], adds: [{ parentId: 3, nextId: null, node: addNode }] } };
  return [
    { type: 4, timestamp: t0, data: { href: "https://shop.example/checkout", width: 1280, height: 800 } },
    full as RrwebEvent,
    { type: 3, timestamp: t0 + 20, data: { source: 5, text: SECRET_TEXT, isChecked: false, id: 4 } },
    mutation as RrwebEvent,
    { type: 3, timestamp: t0 + 40, data: { source: 2, type: 2, id: 7, x: 10, y: 20 } },
    { type: 3, timestamp: t0 + 50, data: { source: 2, type: 2, id: 7, x: 11, y: 21 } },
  ];
}

function snapshotPayload(token: string, distinctId: string, sessionId: string, events: RrwebEvent[], windowId = "w1") {
  return [{ event: "$snapshot", properties: { $snapshot_data: events, $session_id: sessionId, $window_id: windowId, $snapshot_bytes: 1000, distinct_id: distinctId, token, $lib: "web" } }];
}

async function sendRecording(token: string, distinctId: string, sessionId: string, events: RrwebEvent[], windowId = "w1") {
  const body = gzipSync(strToU8(JSON.stringify(snapshotPayload(token, distinctId, sessionId, events, windowId))));
  return fetch(`${run.base}/s/?compression=gzip-js&ip=0&_=${Date.now()}&ver=1.435.1`, { method: "POST", headers: { "content-type": "text/plain" }, body });
}

const objectText = () => [...store.objects.values()].map((o) => new TextDecoder().decode(gunzipSync(o.body))).join("\n");

before(async () => {
  if (skipNoPg) return;
  useReplayStore(store);
  pg = await migratedPg();
  run = await startService(pg, [site, other], { consoleSecret: SECRET, now: () => new Date(clock) });
});

test("decompress, mask and count: recorder events come back plain, with every typed value masked", () => {
  const t0 = Date.now();
  const events = pageEvents(t0, { compressed: true }).map((e) => maskEvent(decompressEvent(e))).filter((e): e is RrwebEvent => e !== null);
  const text = JSON.stringify(events);
  assert.doesNotMatch(text, new RegExp(SECRET_TEXT));
  assert.doesNotMatch(text, /"cv"/);
  assert.match(text, /Visible heading/, "page text is kept");
  assert.match(text, /"value":"Buy now"/, "button labels are kept");
  assert.match(text, /\*{17}/);
  const s = statsOf(events);
  assert.deepEqual({ clicks: s.clicks, keypresses: s.keypresses, urls: s.urls, first: s.firstTs, last: s.lastTs }, { clicks: 2, keypresses: 1, urls: ["https://shop.example/checkout"], first: t0, last: t0 + 50 });
  assert.throws(() => decompressEvent({ type: 2, timestamp: 1, data: "not gzip", cv: "2024-10" }), /not valid gzip/);
  assert.throws(() => extractSnapshots([{ event: "$snapshot", properties: { $snapshot_data: [], $session_id: "../../etc" } }]), /session id/);
  assert.deepEqual(extractSnapshots([{ event: "$pageview", properties: {} }]), []);
});

test("whatever the app's own recorder settings, network bodies and headers, console logs and mobile input values are not kept", () => {
  const t0 = Date.now();
  // The browser SDK's local config (session_recording.recordBody / recordHeaders,
  // enable_recording_console_log) wins over the project's config, so the
  // service strips these itself.
  const network = {
    type: 6,
    timestamp: t0,
    data: {
      plugin: "rrweb/network@1",
      payload: {
        requests: [
          {
            name: "https://shop.example/api/login", method: "POST", status: 200, initiatorType: "fetch", duration: 12,
            requestHeaders: { authorization: `Bearer ${SECRET_TEXT}` },
            requestBody: JSON.stringify({ email: "a@b.example", password: SECRET_TEXT }),
            responseHeaders: { "set-cookie": SECRET_TEXT },
            responseBody: JSON.stringify({ token: SECRET_TEXT }),
          },
        ],
      },
    },
  };
  const consoleLog = { type: 6, timestamp: t0 + 1, data: { plugin: "rrweb/console@1", payload: { level: "log", trace: [], payload: [`"password ${SECRET_TEXT}"`] } } };
  const mobileFull = {
    type: 2,
    timestamp: t0 + 2,
    data: {
      initialOffset: { top: 0, left: 0 },
      wireframes: [
        {
          id: 1, type: "div", x: 0, y: 0, width: 400, height: 800,
          childWireframes: [
            { id: 2, type: "input", inputType: "password", value: SECRET_TEXT, x: 0, y: 0, width: 100, height: 20 },
            { id: 3, type: "input", inputType: "button", value: "Sign in", x: 0, y: 30, width: 100, height: 20 },
            { id: 4, type: "text", text: "Visible heading", x: 0, y: 60, width: 100, height: 20 },
          ],
        },
      ],
    },
  };
  const mobileMutation = {
    type: 3,
    timestamp: t0 + 3,
    data: { source: 0, adds: [{ parentId: 1, wireframe: { id: 5, type: "input", inputType: "text_area", value: SECRET_TEXT } }], updates: [{ wireframe: { id: 2, type: "input", inputType: "text", value: SECRET_TEXT } }] },
  };
  const batches = extractSnapshots(snapshotPayload("tok", "d1", "s-mask", [network, consoleLog, mobileFull, mobileMutation] as RrwebEvent[]));
  const text = JSON.stringify(batches);
  assert.doesNotMatch(text, new RegExp(SECRET_TEXT));
  assert.doesNotMatch(text, /rrweb\/console@1/, "console logs are dropped");
  assert.match(text, /shop\.example\/api\/login/, "request timing and addresses are kept");
  assert.match(text, /"duration":12/);
  assert.match(text, /Sign in/, "mobile button labels are kept");
  assert.match(text, /Visible heading/, "mobile page text is kept");
  assert.equal(batches[0].events.length, 3);
});

test("recording off: nothing is stored and the app's config says so", { skip: skipNoPg }, async () => {
  const cfg = (await (await fetch(`${run.base}/array/${site.token}/config`)).json()) as { sessionRecording: unknown };
  assert.equal(cfg.sessionRecording, false);
  const res = await sendRecording(site.token, "u1", "sess-off", pageEvents(clock));
  assert.equal(res.status, 200);
  assert.equal(store.objects.size, 0);
  assert.equal(pg.psql("select count(*) from pa.replays"), "0");
  assert.equal((await api(run, site, "/recording-settings", { method: "PUT", body: { replay_enabled: true }, role: "viewer" })).status, 403);
  assert.equal((await api(run, site, "/recording-settings", { method: "PUT", body: { replay_sample_rate: 3 } })).status, 400);
});

test("recording on: chunks in object storage, an index row, and inputs masked in the stored bytes", { skip: skipNoPg }, async () => {
  const put = await api(run, site, "/recording-settings", { method: "PUT", body: { replay_enabled: true, replay_sample_rate: 0.25 } });
  assert.equal(put.status, 200, JSON.stringify(put.body));
  const cfg = (await (await fetch(`${run.base}/array/${site.token}/config`)).json()) as { sessionRecording: { endpoint: string; sampleRate: string; masking: { maskAllInputs: boolean } } };
  assert.equal(cfg.sessionRecording.endpoint, "/s/");
  assert.equal(cfg.sessionRecording.sampleRate, "0.25");
  assert.equal(cfg.sessionRecording.masking.maskAllInputs, true);
  assert.equal(((await (await fetch(`${run.base}/array/${other.token}/config`)).json()) as { sessionRecording: unknown }).sessionRecording, false, "another app is unaffected");

  const t0 = clock - 60_000;
  assert.equal((await sendRecording(site.token, "visitor-1", "sess-1", pageEvents(t0, { compressed: true }))).status, 200);
  assert.equal((await sendRecording(site.token, "visitor-1", "sess-1", [{ type: 3, timestamp: t0 + 5000, data: { source: 1, positions: [{ x: 1, y: 2, id: 3, timeOffset: 0 }] } }])).status, 200);
  assert.equal(store.objects.size, 2);
  for (const o of store.objects.values()) {
    assert.equal(o.body[0], 0x1f, "gzip");
    assert.equal(Buffer.from(o.body).includes(Buffer.from(SECRET_TEXT)), false, "not in the raw stored bytes");
  }
  const text = objectText();
  assert.equal(text.includes(SECRET_TEXT), false, "not in the decompressed stored bytes either");
  assert.match(text, /Visible heading/);
  const row = pg.psql(`select distinct_id, click_count, keypress_count, event_count, chunk_count, urls[1], ended_at - started_at from pa.replays where project_id = '${site.project_id}' and session_id = 'sess-1'`);
  assert.equal(row, "visitor-1|2|1|7|2|https://shop.example/checkout|00:00:05");
  assert.equal(pg.psql(`select string_agg(seq::text, ',' order by seq) from pa.replay_chunks where session_id = 'sess-1'`), "1,2");
  const expires = pg.psql(`select extract(epoch from expires_at - started_at) / 86400 from pa.replays where session_id = 'sess-1'`);
  assert.equal(Number(expires), REPLAY_RETENTION_DAYS);
  assert.equal((await sendRecording("site_unknown0000000", "x", "s", pageEvents(t0))).status, 401);
  assert.equal((await fetch(`${run.base}/s/`, { method: "POST", body: "{not json" })).status, 400);
});

test("a recording longer than the maximum stops being stored", { skip: skipNoPg }, async () => {
  const t0 = clock - MAX_RECORDING_MS - 120_000;
  await sendRecording(site.token, "long-visitor", "sess-long", pageEvents(t0));
  const late = [{ type: 3, timestamp: t0 + MAX_RECORDING_MS + 60_000, data: { source: 1, positions: [] } }];
  await sendRecording(site.token, "long-visitor", "sess-long", late);
  assert.equal(pg.psql("select chunk_count || ',' || dropped_chunks from pa.replays where session_id = 'sess-long'"), "1,1");
});

test("a recording opens from a person, an event and a list of people; other projects see nothing", { skip: skipNoPg }, async () => {
  // visitor-1 later identifies as buyer@shop.example (one person, two ids).
  const t = new Date(clock - 30_000);
  await fetch(`${run.base}/batch/`, {
    method: "POST",
    body: JSON.stringify({ api_key: site.token, batch: [{ event: "$identify", distinct_id: "buyer@shop.example", properties: { $anon_distinct_id: "visitor-1", $session_id: "sess-1" }, timestamp: t.toISOString(), uuid: "0192a0a0-0000-7000-8000-000000000001" }] }),
  });
  await run.svc.deps.writer.flush();
  const byPerson = await api(run, site, "/replays?distinct_id=buyer%40shop.example");
  assert.deepEqual(byPerson.body.replays.map((r: { session_id: string }) => r.session_id), ["sess-1"]);
  const personId = pg.psql(`select person_id from pa.person_distinct_ids where project_id = '${site.project_id}' and distinct_id = 'buyer@shop.example'`);
  assert.equal((await api(run, site, `/replays?person_id=${personId}`)).body.replays.length, 1);

  const [eventId] = await insertEvents(pg, site.project_id, [{ event: "purchase", distinct_id: "visitor-1", timestamp: new Date(clock - 60_000 + 2_000), session_id: "sess-1" }]);
  const byEvent = await api(run, site, `/replays?event=${eventId}`);
  assert.equal(byEvent.body.replays[0].session_id, "sess-1");
  assert.equal(byEvent.body.offset_ms, 2_000);
  assert.equal((await api(run, site, "/replays?event=00000000-0000-4000-8000-000000000000")).status, 404);

  const dropOffs = await api(run, site, "/replays/for-people", { body: { person_ids: [personId, "00000000-0000-4000-8000-000000000001"] } });
  assert.deepEqual(dropOffs.body.replays.map((r: { session_id: string }) => r.session_id), ["sess-1"]);
  assert.equal((await api(run, site, "/replays/for-people", { body: { person_ids: [] } })).status, 400);

  const latest = await api(run, site, "/replays");
  assert.deepEqual(latest.body.replays.map((r: { session_id: string }) => r.session_id).sort(), ["sess-1", "sess-long"]);
  assert.equal((await api(run, other, "/replays")).body.replays.length, 0);
  assert.equal((await api(run, other, "/replays/sess-1")).status, 404);
});

test("the player link is signed for 10 minutes and only opens its own recording", { skip: skipNoPg }, async () => {
  const link = await api(run, site, "/replays/sess-1/link", { body: { offset_ms: 2000 } });
  assert.equal(link.status, 200);
  const url = new URL(link.body.url);
  assert.equal(url.pathname, "/player/replay");
  assert.equal(url.searchParams.get("t"), "2000");
  assert.equal(Date.parse(link.body.expires_at) - clock <= 600_000, true);
  const page = await fetch(url);
  assert.equal(page.status, 200);
  const csp = page.headers.get("content-security-policy") ?? "";
  assert.match(csp, /script-src 'self'/);
  assert.match(csp, /frame-ancestors/);
  assert.equal(page.headers.get("referrer-policy"), "no-referrer");
  const html = await page.text();
  assert.doesNotMatch(html, /<script>/, "no inline script");
  assert.match(html, /data-offset="2000"/);
  const eventsPath = /data-events="([^"]+)"/.exec(html)![1].replace(/&amp;/g, "&");
  const lines = (await (await fetch(`${run.base}${eventsPath}`)).text()).trim().split("\n").map((l) => JSON.parse(l));
  assert.equal(lines.length, 7);
  assert.equal(lines[0].window_id, "w1");
  assert.equal(JSON.stringify(lines).includes(SECRET_TEXT), false);

  const tampered = new URL(url);
  tampered.searchParams.set("r", "replay:sess-long");
  assert.equal((await fetch(tampered)).status, 403);
  const events = new URL(`${run.base}${eventsPath}`);
  events.searchParams.set("p", other.project_id);
  assert.equal((await fetch(events)).status, 403);
  const saved = clock;
  clock += 601_000;
  try {
    assert.equal((await fetch(url)).status, 403, "expired after 10 minutes");
  } finally {
    clock = saved;
  }
  assert.equal((await api(run, other, "/replays/sess-1/link", { body: {} })).status, 404);
});

test("the SDK's lazily loaded code and the player files are served; nothing else is", { skip: skipNoPg }, async () => {
  const rec = await fetch(`${run.base}/static/lazy-recorder.js?v=1.435.1`);
  assert.equal(rec.status, 200);
  assert.match(rec.headers.get("content-type") ?? "", /javascript/);
  assert.equal((await fetch(`${run.base}/static/1.435.1/surveys.js`)).status, 200);
  assert.equal((await fetch(`${run.base}/static/array.js`)).status, 404);
  assert.equal((await fetch(`${run.base}/player/assets/replay.js`)).status, 200);
  assert.equal((await fetch(`${run.base}/player/assets/vendor/rrweb-player/rrweb-player.min.js`)).status, 200);
  assert.equal((await fetch(`${run.base}/player/assets/..%2F..%2Fsrc%2Fconfig.ts`)).status, 404);
  assert.equal((await fetch(`${run.base}/player/assets/sdk/LICENSE`)).status, 404);
});

test("a person's deletion removes their recordings and objects; the export lists them first", { skip: skipNoPg }, async () => {
  const headers = { authorization: `Bearer ${await (await import("./helpers.ts")).token(site)}` };
  const exp = (await (await fetch(`${run.base}/v1/projects/${site.project_id}/persons/buyer%40shop.example/export`, { headers })).json()) as { replays: { session_id: string }[] };
  assert.deepEqual(exp.replays.map((r) => r.session_id), ["sess-1"]);
  const before = store.objects.size;
  const del = await fetch(`${run.base}/v1/projects/${site.project_id}/persons/buyer%40shop.example/delete`, { method: "POST", headers });
  assert.equal(del.status, 202);
  await runDueJobs(run.svc.deps.db, run.svc.deps.hooks, silentLogger, new Date(clock + 25 * 3600_000));
  assert.equal(pg.psql("select count(*) from pa.replays where session_id = 'sess-1'"), "0");
  assert.equal(pg.psql("select count(*) from pa.replay_chunks where session_id = 'sess-1'"), "0");
  assert.equal(store.objects.size, before - 2);
  assert.match(pg.psql(`select result::text from pa.gdpr_jobs where distinct_id = 'buyer@shop.example'`), /"replays": 1/);
  assert.equal(pg.psql("select count(*) from pa.replays where session_id = 'sess-long'"), "1", "others are kept");
});

test("recordings expire after the plan's replay days", { skip: skipNoPg }, async () => {
  const n = store.objects.size;
  assert.equal(await expireReplays(run.svc.deps.db, store, new Date(clock + (REPLAY_RETENTION_DAYS - 2) * 86_400_000)), 0, "nothing inside the window");
  assert.equal(await expireReplays(run.svc.deps.db, store, new Date(clock + (REPLAY_RETENTION_DAYS + 1) * 86_400_000)), 1);
  assert.equal(pg.psql("select count(*) from pa.replays"), "0");
  assert.ok(store.objects.size < n);
  assert.equal(store.objects.size, 0);
});
