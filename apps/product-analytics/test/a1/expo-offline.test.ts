// The unmodified React Native SDK (Node, with a react-native stand-in)
// through @railyard/analytics-js/react-native: $screen once per route
// change; an app offline for 10 minutes keeps its queue on the device,
// survives a restart, and every event is stored exactly once, even when a
// response is lost and the batch is sent again.
import { before, mock, test } from "node:test";
import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { makeSite, migratedPg, skipNoPg, sleep, startService, type Running, type ScratchPg } from "./helpers.ts";

const SHIM = pathToFileURL(join(dirname(fileURLToPath(import.meta.url)), "fixtures/react-native-shim.cjs")).href;
registerHooks({
  resolve(specifier, context, next) {
    if (specifier === "react-native") return { url: SHIM, shortCircuit: true, format: "commonjs" };
    return next(specifier, context);
  },
});

let pg: ScratchPg;
let run: Running;
const site = makeSite();

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site]);
});

type Mode = "online" | "offline" | "lose-response";
let mode: Mode = "online";
let sent = 0;
const realFetch = globalThis.fetch;

test("offline for 10 minutes, restarted, then online: every event stored exactly once; screens on navigation", { skip: skipNoPg }, async () => {
  globalThis.fetch = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input instanceof Request ? input.url : input);
    if (url.startsWith(run.base) && url.includes("/batch")) sent += 1;
    if (mode === "offline") throw new TypeError("Network request failed");
    const res = await realFetch(input, init);
    if (mode === "lose-response" && url.includes("/batch")) {
      mode = "online";
      throw new TypeError("Network request failed");
    }
    return res;
  }) as typeof fetch;
  const { createNativeAnalytics, screenTracker } = await import("../../../../packages/analytics-js/src/react-native.mjs");
  const disk = new Map<string, string>();
  const customStorage = { getItem: (k: string) => disk.get(k) ?? null, setItem: (k: string, v: string) => void disk.set(k, v) };
  const sdkOptions = { customStorage, flushAt: 1000, flushInterval: 0, fetchRetryCount: 0, fetchRetryDelay: 10, captureAppLifecycleEvents: false, preloadFeatureFlags: false };

  // Before consent there is no client at all.
  const pending = createNativeAnalytics({ site: site.token, host: run.base, sdkOptions });
  pending.capture("before_consent");
  assert.equal(pending.client, null);

  mode = "offline";
  const t0 = Date.parse("2026-09-30T08:00:00Z");
  mock.timers.enable({ apis: ["Date"], now: t0 });
  try {
    const app = createNativeAnalytics({ site: site.token, host: run.base, consent: "granted", sdkOptions });
    const track = screenTracker(app);
    assert.equal(track("/"), true);
    assert.equal(track("/"), false, "same route, no second $screen");
    assert.equal(track("/cart"), true);
    for (let minute = 0; minute <= 10; minute += 1) {
      app.capture("offline_tick", { minute });
      await app.flush().catch(() => undefined);
      mock.timers.tick(60_000);
    }
    await sleep(400); // the SDK writes its queue to the device after 100 ms
  } finally {
    mock.timers.reset();
  }
  assert.ok(sent >= 11, "the app tried to send while offline");
  assert.equal(Number(pg.psql(`select count(*) from pa.events where project_id = '${site.project_id}'`)), 0);

  // The app is killed and started again with the same device storage, now online;
  // the first batch's response is lost, so the batch is sent twice.
  mode = "lose-response";
  const restarted = createNativeAnalytics({ site: site.token, host: run.base, consent: "granted", sdkOptions });
  await sleep(200);
  await restarted.flush().catch(() => undefined);
  await restarted.flush();
  await restarted.shutdown();
  await run.svc.deps.writer.flush();

  const rows = pg.psql(`select event || ':' || coalesce(properties ->> 'minute', properties ->> '$screen_name') from pa.events where project_id = '${site.project_id}' order by "timestamp", event`).split("\n");
  assert.deepEqual(rows, ["$screen:/", "$screen:/cart", ...Array.from({ length: 11 }, (_, m) => `offline_tick:${m}`)]);
  assert.equal(pg.psql(`select count(distinct uuid) from pa.events where project_id = '${site.project_id}'`), "13");
  assert.equal(pg.psql(`select received || '|' || stored from pa.usage_monthly where project_id = '${site.project_id}'`), "26|13", "the batch arrived twice and was stored once");
  // Timestamps keep the device's time (10 minutes apart), corrected for the send delay.
  const span = Number(pg.psql(`select extract(epoch from max("timestamp") - min("timestamp"))::int from pa.events where project_id = '${site.project_id}' and event = 'offline_tick'`));
  assert.equal(span, 600);
  assert.equal(pg.psql(`select properties ->> '$lib' from pa.events where project_id = '${site.project_id}' limit 1`), "posthog-react-native");
  globalThis.fetch = realFetch;
});
