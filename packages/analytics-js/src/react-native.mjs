// @railyard/analytics-js for Expo and React Native. Wraps the maintained
// open-source React Native SDK (see NOTICE) with Railyard's host, the app's
// site key, an on-device queue that survives being offline, and privacy
// defaults (nothing is created or sent before consent; recordings off; when
// on, inputs, images and sandboxed views are masked; no network telemetry).

import { PostHog } from "posthog-react-native";
import { checkConsent, resolveSite } from "./env.mjs";

export { DEFAULT_HOST, readAnalyticsEnv } from "./env.mjs";

export const NATIVE_PRIVACY_DEFAULTS = Object.freeze({
  personProfiles: "identified_only",
  enableSessionReplay: false,
  sessionReplayConfig: Object.freeze({ maskAllTextInputs: true, maskAllImages: true, maskAllSandboxedViews: true, captureLog: false, captureNetworkTelemetry: false }),
  captureAppLifecycleEvents: true,
});

export function createNativeAnalytics(options = {}) {
  const { site, host } = resolveSite(options);
  const requireConsent = options.requireConsent ?? true;
  let consent = checkConsent(options.consent ?? (requireConsent ? "pending" : "granted"));
  let client = null;

  const start = () => {
    if (client) {
      void client.optIn();
      return;
    }
    client = new PostHog(site, {
      ...NATIVE_PRIVACY_DEFAULTS,
      host,
      persistence: "file",
      flushAt: 20,
      flushInterval: 10_000,
      ...(options.sdkOptions ?? {}),
    });
  };
  if (!requireConsent || consent === "granted") start();

  const whenOn = (fn) => (...args) => (client && consent !== "denied" ? fn(client, ...args) : undefined);

  return {
    site,
    host,
    get consent() {
      return consent;
    },
    setConsent(state) {
      consent = checkConsent(state);
      if (consent === "granted") start();
      if (consent === "denied" && client) {
        void client.optOut();
        client.reset();
      }
    },
    capture: whenOn((c, event, properties) => c.capture(event, properties)),
    screen: whenOn((c, name, properties) => c.screen(name, properties)),
    identify: whenOn((c, distinctId, properties) => c.identify(distinctId, properties)),
    alias: whenOn((c, alias) => c.alias(alias)),
    group: whenOn((c, type, key, properties) => c.group(type, key, properties)),
    reset: whenOn((c) => c.reset()),
    /** Sends what is queued now (the queue is also sent on a timer and when full). */
    flush: async () => (client ? client.flush() : undefined),
    shutdown: async () => (client ? client.shutdown() : undefined),
    isEnabled: (flag) => (client ? client.isFeatureEnabled(flag) === true : false),
    get client() {
      return client;
    },
  };
}

/**
 * Screen tracking for expo-router or React Navigation: call the returned
 * function with the current route on every navigation; it sends one
 * $screen per change of route.
 */
export function screenTracker(analytics) {
  let last = null;
  return (route, properties) => {
    if (typeof route !== "string" || !route || route === last) return false;
    last = route;
    analytics.screen(route, properties);
    return true;
  };
}
