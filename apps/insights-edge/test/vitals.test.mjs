// QY1.2 (P-Y12): the /s.js page tag sends LCP, INP and CLS (and FCP, TTFB)
// within 10% of Chrome's field values for every load in the shared fixture
// (packages/insights-web/test/fixtures/chrome-page-loads.json), once per page
// view, and the whole tag stays at or under 3 KB gzip.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { gzipSync } from "node:zlib";
import { TRACKER_JS } from "../src/tracker.mjs";

const fixture = JSON.parse(readFileSync(new URL("../../../packages/insights-web/test/fixtures/chrome-page-loads.json", import.meta.url), "utf8"));
const METRICS = ["lcp", "inp", "cls", "fcp", "ttfb"];
const within10 = (got, want) => typeof got === "number" && (want === 0 ? got === 0 : Math.abs(got - want) / want <= 0.1);

/** Runs the tag in a fake page whose performance observers replay `entries`. */
function page({ entries = [], interactionCount, supported = ["navigation", "paint", "largest-contentful-paint", "layout-shift", "event", "first-input"], attrs = {} } = {}) {
  const sent = [];
  const listeners = {};
  const win = {};
  const loc = { pathname: "/", search: "", hostname: "shop.example" };
  const doc = { currentScript: { getAttribute: (n) => ({ "data-site": "ry_site_abcdefgh", ...attrs })[n] ?? null, src: "https://c.test/s.js" }, readyState: "complete", referrer: "", title: "T", visibilityState: "visible" };
  class PO {
    static supportedEntryTypes = supported;
    constructor(cb) {
      this.cb = cb;
    }
    observe({ type }) {
      if (!supported.includes(type)) throw new TypeError("unsupported");
      const mine = entries.filter((e) => e.entryType === type);
      if (mine.length) this.cb({ getEntries: () => mine });
    }
  }
  const ctx = {
    document: doc,
    location: loc,
    navigator: { doNotTrack: "0", language: "en", sendBeacon: (_u, blob) => (sent.push(JSON.parse(blob.text)), true) },
    screen: { width: 1, height: 1 },
    history: { pushState() {} },
    addEventListener: (n, f) => ((listeners[n] ??= []).push(f)),
    Blob: class {
      constructor(parts) {
        this.text = parts.join("");
      }
    },
    URL,
    fetch: () => Promise.resolve(),
    window: win,
    PerformanceObserver: PO,
    performance: { interactionCount },
  };
  new Function(...Object.keys(ctx), TRACKER_JS)(...Object.values(ctx));
  const fire = (n) => (listeners[n] ?? []).forEach((f) => f());
  return {
    sent,
    win,
    loc,
    hide() {
      doc.visibilityState = "hidden";
      fire("visibilitychange");
    },
    fire,
  };
}

for (const load of fixture.loads) {
  test(`/s.js vitals within 10% of Chrome: ${load.name}`, () => {
    const p = page({ entries: load.entries, interactionCount: load.interactionCount });
    p.hide();
    const perf = p.sent.filter((b) => b.type === "performance");
    assert.equal(perf.length, 1, "one performance event per page view");
    for (const m of METRICS) {
      const want = load.chrome[m];
      if (want === undefined) assert.equal(perf[0].payload[m], undefined, `${m} should be absent`);
      else assert.ok(within10(perf[0].payload[m], want), `${m}: got ${perf[0].payload[m]}, Chrome ${want}`);
    }
    assert.equal(perf[0].payload.website, "ry_site_abcdefgh");
    assert.equal(perf[0].payload.url, "/");
  });
}

test("/s.js sends the metrics once: hide then pagehide is one event", () => {
  const p = page({ entries: fixture.loads[0].entries });
  p.hide();
  p.fire("pagehide");
  assert.equal(p.sent.filter((b) => b.type === "performance").length, 1);
  assert.equal(p.sent.filter((b) => b.type === "event").length, 1, "the page view itself");
});

test("/s.js: a client-side route change sends the previous page's metrics under the previous URL first", () => {
  const p = page({ entries: fixture.loads[0].entries });
  p.loc.pathname = "/pricing";
  p.fire("popstate");
  const types = p.sent.map((b) => [b.type, b.payload.url]);
  assert.deepEqual(types, [["event", "/"], ["performance", "/"], ["event", "/pricing"]]);
});

test("/s.js: no vitals without performance observers, with data-vitals=off, or without any metric", () => {
  const off = page({ entries: fixture.loads[0].entries, attrs: { "data-vitals": "off" } });
  off.hide();
  assert.equal(off.sent.filter((b) => b.type === "performance").length, 0);
  const none = page({ entries: [], supported: [] });
  none.hide();
  assert.equal(none.sent.filter((b) => b.type === "performance").length, 0);
});

test("/s.js: a browser without layout-shift reporting sends no made-up CLS", () => {
  const p = page({ entries: fixture.loads[3].entries, supported: ["navigation", "paint", "largest-contentful-paint"] });
  p.hide();
  const perf = p.sent.find((b) => b.type === "performance");
  assert.equal(perf.payload.cls, undefined);
  assert.equal(typeof perf.payload.lcp, "number");
});

test("/s.js stays at or under 3 KB gzip", () => {
  const size = gzipSync(Buffer.from(TRACKER_JS)).length;
  assert.ok(size <= 3072, `the page tag is ${size} bytes gzip`);
});
