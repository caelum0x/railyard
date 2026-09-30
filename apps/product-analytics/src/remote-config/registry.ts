// Remote config (/array/:token/config and config.js), assembled from a
// provider registry: the core provider (capture switches from the project's
// settings) first, then each section provider in sections.ts. A later
// provider's keys win. Nothing here turns on recording by itself.

import type { ProjectSettings } from "../tenancy/settings.ts";
import type { Site } from "../tenancy/site-directory.ts";

export interface ConfigContext {
  site: Site;
  settings: ProjectSettings;
}

export type ConfigProvider = (ctx: ConfigContext) => Record<string, unknown> | Promise<Record<string, unknown>>;

export const coreProvider: ConfigProvider = ({ site, settings }) => ({
  token: site.token,
  supportedCompression: ["gzip", "gzip-js"],
  hasFeatureFlags: settings.flags_client_key !== null,
  analytics: { endpoint: "/i/v0/e/" },
  elementsChainAsString: true,
  autocapture_opt_out: !settings.autocapture,
  autocaptureExceptions: false,
  captureDeadClicks: false,
  capturePerformance: false,
  defaultIdentifiedOnly: true,
  sessionRecording: false,
  heatmaps: false,
  surveys: false,
  siteApps: [],
});

export async function assembleConfig(ctx: ConfigContext, sections: ConfigProvider[]): Promise<Record<string, unknown>> {
  let out: Record<string, unknown> = { ...(await coreProvider(ctx)) };
  for (const p of sections) out = { ...out, ...(await p(ctx)) };
  return out;
}

/** The script form some SDK builds load before the JSON form. */
export function configScript(token: string, config: Record<string, unknown>): string {
  const t = JSON.stringify(token);
  return `(function(){window._POSTHOG_REMOTE_CONFIG=window._POSTHOG_REMOTE_CONFIG||{};window._POSTHOG_REMOTE_CONFIG[${t}]={config:${JSON.stringify(config)},siteApps:[]};})();`;
}
