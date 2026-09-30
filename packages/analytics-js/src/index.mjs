// @railyard/analytics-js for the web. Wraps the maintained open-source
// browser SDK (see NOTICE) with Railyard's host, the app's site key and
// privacy defaults:
//   - nothing is loaded, stored or sent until consent is granted (default:
//     consent required; pass { requireConsent: false } where no consent is needed);
//   - recordings mask every input, mask text under [data-private] and
//     .ph-mask, block [data-block], never record canvas, headers or bodies;
//   - no console-log capture and no performance capture.

import * as sdkModule from "posthog-js";
import { checkConsent, readAnalyticsEnv as readProcessEnv, resolveSite } from "./env.mjs";

export { DEFAULT_HOST } from "./env.mjs";

// Bundlers load the ES build (default export = the SDK); Node loads the CommonJS build (named export).
const sdk = typeof sdkModule.default?.init === "function" ? sdkModule.default : sdkModule.posthog ?? sdkModule.default?.default;

/** Vite inlines import.meta.env.VITE_* at build time. */
function viteEnv() {
  try {
    const env = import.meta.env;
    return env ? { site: env.VITE_ANALYTICS_SITE, host: env.VITE_ANALYTICS_HOST } : {};
  } catch {
    return {};
  }
}

/** `{site, host}` from NEXT_PUBLIC_, EXPO_PUBLIC_ or VITE_ variables. */
export function readAnalyticsEnv() {
  return readProcessEnv(viteEnv());
}

export const PRIVACY_DEFAULTS = Object.freeze({
  person_profiles: "identified_only",
  mask_all_text: false,
  mask_all_element_attributes: false,
  enable_recording_console_log: false,
  capture_performance: false,
  respect_dnt: true,
  session_recording: Object.freeze({
    maskAllInputs: true,
    maskTextSelector: "[data-private], .ph-mask",
    blockSelector: "[data-block]",
    recordCrossOriginIframes: false,
    recordHeaders: false,
    recordBody: false,
    captureCanvas: Object.freeze({ recordCanvas: false }),
  }),
});

let seq = 0;

/**
 * Creates one analytics client. Calls made before consent is granted are
 * dropped, not queued: nothing about a visitor leaves the page before then.
 */
export function createAnalytics(options = {}) {
  const { site, host } = resolveSite(options, viteEnv());
  const requireConsent = options.requireConsent ?? true;
  let consent = checkConsent(options.consent ?? (requireConsent ? "pending" : "granted"));
  const name = options.name ?? `railyard_${(seq += 1)}`;
  let client = null;
  const flagListeners = [];

  const start = () => {
    if (client) {
      client.opt_in_capturing();
      return;
    }
    const config = {
      ...PRIVACY_DEFAULTS,
      session_recording: { ...PRIVACY_DEFAULTS.session_recording, ...(options.recording ?? {}) },
      api_host: host,
      persistence: "localStorage+cookie",
      capture_pageview: options.capturePageview ?? "history_change",
      capture_pageleave: options.capturePageleave ?? true,
      autocapture: options.autocapture ?? true,
      debug: options.debug ?? false,
      ...(options.sdkOptions ?? {}),
    };
    client = sdk.init(site, config, name);
    for (const cb of flagListeners) client.onFeatureFlags(cb);
  };

  if (!requireConsent || consent === "granted") start();

  const whenOn = (fn) => (...args) => (client && consent !== "denied" ? fn(client, ...args) : undefined);

  return {
    site,
    host,
    get consent() {
      return consent;
    },
    /** From the consent slot: "granted" starts sending; "denied" stops and forgets this visitor on this device. */
    setConsent(state) {
      consent = checkConsent(state);
      if (consent === "granted") start();
      if (consent === "denied" && client) {
        client.opt_out_capturing();
        client.reset(true);
      }
    },
    capture: whenOn((c, event, properties) => c.capture(event, properties)),
    identify: whenOn((c, distinctId, set, setOnce) => c.identify(distinctId, set, setOnce)),
    alias: whenOn((c, alias) => c.alias(alias)),
    group: whenOn((c, type, key, properties) => c.group(type, key, properties)),
    setPersonProperties: whenOn((c, set, setOnce) => c.setPersonProperties(set, setOnce)),
    reset: whenOn((c) => c.reset()),
    isEnabled: (flag) => (client ? client.isFeatureEnabled(flag) === true : false),
    flagValue: (flag) => (client ? client.getFeatureFlagPayload(flag) ?? null : null),
    onFlags(cb) {
      flagListeners.push(cb);
      if (client) client.onFeatureFlags(cb);
    },
    /** The underlying SDK client once consent is granted, else null. */
    get client() {
      return client;
    },
  };
}
