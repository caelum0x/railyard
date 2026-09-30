// Where the app's site key and the analytics host come from. The names are
// the ones `scribase add analytics` writes. Each is written out in full so
// Next.js and Expo can inline it at build time; Vite's are read by the web
// entry (import.meta is not available in every React Native bundler).

export const DEFAULT_HOST = "https://pa.railyard.ereboslab.com";

const clean = (v) => (typeof v === "string" && v.trim() ? v.trim() : undefined);

function fromProcess() {
  try {
    return {
      site: clean(process.env.NEXT_PUBLIC_ANALYTICS_SITE) ?? clean(process.env.EXPO_PUBLIC_ANALYTICS_SITE),
      host: clean(process.env.NEXT_PUBLIC_ANALYTICS_HOST) ?? clean(process.env.EXPO_PUBLIC_ANALYTICS_HOST),
    };
  } catch {
    return {};
  }
}

/** `{site, host}` from the build environment; either may be undefined. `extra` adds bundler-only sources (Vite). */
export function readAnalyticsEnv(extra = {}) {
  const p = fromProcess();
  return { site: p.site ?? clean(extra.site), host: p.host ?? clean(extra.host) };
}

export const SITE_KEY = /^[A-Za-z0-9_-]{8,128}$/;

export function resolveSite(options, extra = {}) {
  const env = readAnalyticsEnv(extra);
  const site = options.site ?? env.site;
  if (!site || !SITE_KEY.test(site)) {
    throw new Error("Analytics needs the app's site key: set NEXT_PUBLIC_ANALYTICS_SITE (or EXPO_PUBLIC_ / VITE_ANALYTICS_SITE), or pass { site }.");
  }
  const host = (options.host ?? env.host ?? DEFAULT_HOST).replace(/\/$/, "");
  if (!/^https?:\/\//.test(host)) throw new Error("The analytics host must be an http(s) URL.");
  return { site, host };
}

export const CONSENT_STATES = ["pending", "granted", "denied"];

export function checkConsent(state) {
  if (!CONSENT_STATES.includes(state)) throw new Error(`Consent must be one of: ${CONSENT_STATES.join(", ")}.`);
  return state;
}
