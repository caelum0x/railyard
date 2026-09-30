// The unmodified browser SDK under jsdom, through @railyard/analytics-js:
// no request (and nothing stored on the device) before consent; after
// consent pageviews, custom events, autocaptured clicks and identify land
// with the right fields; after "denied" nothing more is sent.
import { before, test } from "node:test";
import assert from "node:assert/strict";
import { JSDOM } from "jsdom";
import { makeSite, migratedPg, skipNoPg, sleep, startService, type Running, type ScratchPg } from "./helpers.ts";

let pg: ScratchPg;
let run: Running;
const site = makeSite();
const requests: { url: string; body: string }[] = [];
let dom: JSDOM;

before(async () => {
  if (skipNoPg) return;
  pg = await migratedPg();
  run = await startService(pg, [site]);
  dom = new JSDOM(
    `<!doctype html><html><head><title>Shop</title></head><body>
       <button id="buy" class="btn primary">Buy now</button>
       <input id="email" type="email" value="typed-secret@shop.example">
       <p data-private>private note</p>
     </body></html>`,
    { url: "https://shop.example/products", pretendToBeVisual: true, resources: "usable", runScripts: "dangerously" },
  );
  const w = dom.window as unknown as Record<string, unknown>;
  const realFetch = globalThis.fetch;
  w.fetch = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input instanceof Request ? input.url : input);
    const body = init?.body;
    requests.push({ url, body: typeof body === "string" ? body : body instanceof ArrayBuffer || ArrayBuffer.isView(body) ? `<${(body as ArrayBuffer).byteLength} bytes>` : "" });
    return realFetch(input, init);
  };
  // Record XHR traffic too (the SDK picks fetch or XHR).
  const RealXHR = w.XMLHttpRequest as typeof XMLHttpRequest;
  class RecordingXHR extends RealXHR {
    _url = "";
    open(method: string, url: string | URL, ...rest: unknown[]) {
      this._url = String(url);
      return (super.open as (...a: unknown[]) => void)(method, url, ...rest);
    }
    send(body?: Document | XMLHttpRequestBodyInit | null) {
      requests.push({ url: this._url, body: typeof body === "string" ? body : body ? `<${(body as Blob).size ?? (body as ArrayBuffer).byteLength} bytes>` : "" });
      return super.send(body);
    }
  }
  w.XMLHttpRequest = RecordingXHR;
  w.navigator && Object.defineProperty(w.navigator, "sendBeacon", { value: () => { requests.push({ url: "beacon", body: "" }); return true; }, configurable: true });
  // The SDK reads fetch from globalThis.
  globalThis.fetch = w.fetch as typeof fetch;
  for (const k of ["window", "document", "navigator", "location", "localStorage", "sessionStorage", "HTMLElement", "Element", "Node", "Event", "MouseEvent", "MutationObserver", "XMLHttpRequest", "Blob", "FormData", "screen", "history", "CustomEvent", "getComputedStyle"]) {
    Object.defineProperty(globalThis, k, { value: k === "window" ? dom.window : w[k], configurable: true, writable: true });
  }
});

async function waitFor(check: () => boolean, ms = 15_000) {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    await run.svc.deps.writer.flush();
    if (check()) return;
    await sleep(200);
  }
  assert.fail(`timed out waiting for events; stored: ${events()}; requests: ${JSON.stringify(requests.map((r) => [r.url.replace(run.base, ""), r.body.slice(0, 80)]))}`);
}

// Autocapture starts once the app's config has arrived (script or JSON form).
const autocaptureReady = () => {
  const cfg = (dom.window as unknown as { _POSTHOG_REMOTE_CONFIG?: Record<string, unknown> })._POSTHOG_REMOTE_CONFIG;
  return !!cfg?.[site.token] || requests.some((r) => r.url.includes("/config"));
};
const events = () => pg.psql(`select string_agg(event, ',' order by "timestamp", event) from pa.events where project_id = '${site.project_id}'`);

test("nothing is requested or stored before consent; events flow after it; nothing after denial", { skip: skipNoPg }, async () => {
  const { createAnalytics, PRIVACY_DEFAULTS } = await import("../../../../packages/analytics-js/src/index.mjs");
  const analytics = createAnalytics({ site: site.token, host: run.base });
  assert.equal(analytics.consent, "pending");
  analytics.capture("before_consent");
  analytics.identify("user-web-1");
  (dom.window.document.getElementById("buy") as HTMLElement).click();
  await sleep(1500);
  assert.equal(requests.length, 0, "no request before consent");
  assert.equal(dom.window.localStorage.length, 0, "nothing stored on the device before consent");
  assert.equal(dom.window.document.cookie, "");
  assert.equal(analytics.client, null);

  analytics.setConsent("granted");
  analytics.capture("added_to_cart", { sku: "tee-1" });
  await waitFor(() => /added_to_cart/.test(events()) && /\$pageview/.test(events()) && autocaptureReady());
  await sleep(300);
  (dom.window.document.getElementById("buy") as HTMLElement).dispatchEvent(new dom.window.MouseEvent("click", { bubbles: true, clientX: 5, clientY: 5 }));
  await waitFor(() => /\$autocapture/.test(events()));
  analytics.identify("user-web-1", { email: "web@shop.example" });
  await waitFor(() => /\$identify/.test(events()));

  const pv = pg.psql(`select properties ->> '$current_url' from pa.events where project_id = '${site.project_id}' and event = '$pageview' limit 1`);
  assert.equal(pv, "https://shop.example/products");
  const chain = pg.psql(`select elements_chain from pa.events where project_id = '${site.project_id}' and event = '$autocapture' limit 1`);
  assert.match(chain, /^button/);
  assert.match(chain, /Buy now/);
  assert.doesNotMatch(events(), /before_consent/);
  const p = pg.psql(`select person_id from pa.person_distinct_ids where project_id = '${site.project_id}' and distinct_id = 'user-web-1'`);
  assert.equal(pg.psql(`select properties ->> 'email' from pa.persons where id = '${p}'`), "web@shop.example");
  const resolved = pg.psql(`select count(*) from pa.events e left join pa.person_overrides o on o.project_id = e.project_id and o.old_person_id = e.person_id where e.project_id = '${site.project_id}' and coalesce(o.new_person_id, e.person_id) = '${p}'`);
  assert.equal(resolved, pg.psql(`select count(*) from pa.events where project_id = '${site.project_id}'`), "every event of the visitor counts for the identified person");
  assert.ok(requests.some((r) => r.url.startsWith(`${run.base}/`)));
  assert.ok(requests.every((r) => !r.body.includes("typed-secret")), "input values never leave the page");
  assert.equal(PRIVACY_DEFAULTS.session_recording.maskAllInputs, true);
  assert.equal(analytics.client.config.session_recording.blockSelector, "[data-block]");
  assert.equal(analytics.client.config.session_recording.captureCanvas.recordCanvas, false);

  analytics.setConsent("denied");
  const n = requests.length;
  analytics.capture("after_denied");
  await sleep(2500);
  await run.svc.deps.writer.flush();
  assert.doesNotMatch(events(), /after_denied/);
  assert.ok(requests.slice(n).every((r) => !r.body.includes("after_denied")));
});
