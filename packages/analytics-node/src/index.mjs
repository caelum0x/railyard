// @railyard/analytics-node. Wraps the maintained open-source Node SDK (see
// NOTICE) with Railyard's host and the app's site key. Server events are
// sent in batches; call shutdown() before the process exits.

import { PostHog } from "posthog-node";

export const DEFAULT_HOST = "https://pa.railyard.ereboslab.com";
const SITE_KEY = /^[A-Za-z0-9_-]{8,128}$/;

const clean = (v) => (typeof v === "string" && v.trim() ? v.trim() : undefined);

/** `{site, host}` from ANALYTICS_SITE / NEXT_PUBLIC_ANALYTICS_SITE and PRODUCT_ANALYTICS_URL / ANALYTICS_HOST. */
export function readAnalyticsEnv(env = process.env) {
  return {
    site: clean(env.ANALYTICS_SITE) ?? clean(env.NEXT_PUBLIC_ANALYTICS_SITE) ?? clean(env.EXPO_PUBLIC_ANALYTICS_SITE) ?? clean(env.VITE_ANALYTICS_SITE),
    host: clean(env.PRODUCT_ANALYTICS_URL) ?? clean(env.ANALYTICS_HOST) ?? clean(env.NEXT_PUBLIC_ANALYTICS_HOST),
  };
}

export function createServerAnalytics(options = {}) {
  const env = readAnalyticsEnv(options.env);
  const site = options.site ?? env.site;
  if (!site || !SITE_KEY.test(site)) throw new Error("Analytics needs the app's site key: set ANALYTICS_SITE (or NEXT_PUBLIC_ANALYTICS_SITE), or pass { site }.");
  const host = (options.host ?? env.host ?? DEFAULT_HOST).replace(/\/$/, "");
  const client = new PostHog(site, {
    host,
    flushAt: options.flushAt ?? 20,
    flushInterval: options.flushInterval ?? 10_000,
    // Flags are answered by the service; no personal API key is needed.
    ...(options.sdkOptions ?? {}),
  });
  return {
    site,
    host,
    capture: (distinctId, event, properties, groups) => client.capture({ distinctId, event, properties, groups }),
    identify: (distinctId, set, setOnce) => client.identify({ distinctId, properties: { ...(set ? { $set: set } : {}), ...(setOnce ? { $set_once: setOnce } : {}) } }),
    alias: (distinctId, alias) => client.alias({ distinctId, alias }),
    groupIdentify: (groupType, groupKey, properties) => client.groupIdentify({ groupType, groupKey, properties }),
    isEnabled: async (flag, distinctId) => (await client.isFeatureEnabled(flag, distinctId)) === true,
    flagValue: async (flag, distinctId) => (await client.getFeatureFlagPayload(flag, distinctId)) ?? null,
    flush: () => client.flush(),
    shutdown: () => client.shutdown(),
    client,
  };
}
