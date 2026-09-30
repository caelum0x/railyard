// Unit checks of the wrapper without a browser. The SDK-in-the-loop tests
// (jsdom, Chromium, React Native offline) live in
// apps/product-analytics/test/a1/{sdk-js,browser,expo-offline}.test.ts.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readAnalyticsEnv as readProcessEnv, resolveSite, checkConsent } from "../src/env.mjs";
import { createAnalytics, DEFAULT_HOST, PRIVACY_DEFAULTS, readAnalyticsEnv } from "../src/index.mjs";

const KEY = "ry_site_abcdefghij";

test("site key and host come from NEXT_PUBLIC_ / EXPO_PUBLIC_ names, else options, else the default host", () => {
  const saved = { ...process.env };
  try {
    delete process.env.NEXT_PUBLIC_ANALYTICS_SITE;
    delete process.env.EXPO_PUBLIC_ANALYTICS_SITE;
    delete process.env.NEXT_PUBLIC_ANALYTICS_HOST;
    delete process.env.EXPO_PUBLIC_ANALYTICS_HOST;
    assert.deepEqual(readAnalyticsEnv(), { site: undefined, host: undefined });
    process.env.EXPO_PUBLIC_ANALYTICS_SITE = KEY;
    assert.equal(readProcessEnv().site, KEY);
    process.env.NEXT_PUBLIC_ANALYTICS_SITE = "ry_site_nextnextnext";
    process.env.NEXT_PUBLIC_ANALYTICS_HOST = "https://pa.example.com/";
    assert.deepEqual(resolveSite({}), { site: "ry_site_nextnextnext", host: "https://pa.example.com" });
    assert.deepEqual(resolveSite({ site: KEY, host: "http://localhost:8790" }), { site: KEY, host: "http://localhost:8790" });
    delete process.env.NEXT_PUBLIC_ANALYTICS_HOST;
    assert.equal(resolveSite({}).host, DEFAULT_HOST);
    // Vite's values come in through the web entry.
    delete process.env.NEXT_PUBLIC_ANALYTICS_SITE;
    delete process.env.EXPO_PUBLIC_ANALYTICS_SITE;
    assert.equal(readProcessEnv({ site: "ry_site_vitevitevite" }).site, "ry_site_vitevitevite");
    assert.throws(() => resolveSite({}), /NEXT_PUBLIC_ANALYTICS_SITE/);
    assert.throws(() => resolveSite({ site: "bad key!" }), /site key/);
    assert.throws(() => resolveSite({ site: KEY, host: "ftp://x" }), /http/);
  } finally {
    process.env = saved;
  }
});

test("consent is required by default: no client exists and calls are dropped until granted", () => {
  const a = createAnalytics({ site: KEY, host: "http://127.0.0.1:9" });
  assert.equal(a.consent, "pending");
  assert.equal(a.client, null);
  a.capture("x");
  a.identify("u");
  assert.equal(a.isEnabled("f"), false);
  assert.equal(a.flagValue("f"), null);
  assert.throws(() => a.setConsent("maybe"), /Consent must be/);
  assert.equal(checkConsent("denied"), "denied");
  a.setConsent("denied");
  assert.equal(a.client, null);
});

test("privacy defaults: inputs masked, private text masked, blocked areas left out, no canvas, no headers or bodies", () => {
  assert.equal(PRIVACY_DEFAULTS.session_recording.maskAllInputs, true);
  assert.equal(PRIVACY_DEFAULTS.session_recording.maskTextSelector, "[data-private], .ph-mask");
  assert.equal(PRIVACY_DEFAULTS.session_recording.blockSelector, "[data-block]");
  assert.equal(PRIVACY_DEFAULTS.session_recording.captureCanvas.recordCanvas, false);
  assert.equal(PRIVACY_DEFAULTS.session_recording.recordHeaders, false);
  assert.equal(PRIVACY_DEFAULTS.session_recording.recordBody, false);
  assert.equal(PRIVACY_DEFAULTS.enable_recording_console_log, false);
  assert.equal(PRIVACY_DEFAULTS.capture_performance, false);
  assert.ok(Object.isFrozen(PRIVACY_DEFAULTS.session_recording));
});
