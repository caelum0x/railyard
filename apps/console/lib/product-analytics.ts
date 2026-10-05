// Server-only: what the console asks of the product analytics service on
// behalf of the Scribase bridge (QY1.2, A26). Every call carries a fresh org
// token scoped to one app; the server key is derived here and handed only to
// the Scribase control plane over the signed bridge (Wave Q contract 5.3).
//
// Configuration: PRODUCT_ANALYTICS_URL (how the console reaches the service),
// PRODUCT_ANALYTICS_CONSOLE_SECRET (shared with the service, 32+ characters)
// and PRODUCT_ANALYTICS_PUBLIC_URL (the host apps send events to; defaults to
// DEFAULT_PA_PUBLIC_HOST, the same default the SDKs use).

import { paConfig, paFetch, type PaConfig } from "../app/(app)/product-analytics/_lib/http.ts";
import { hex, hmacSha256 } from "./crypto.ts";

export const DEFAULT_PA_PUBLIC_HOST = "https://pa.railyard.ereboslab.com";

export interface ProductFirstEvent {
  event: string;
  at: string;
}

export interface WebRow {
  value: string;
  visitors: number;
  views: number;
}

export interface WebVital {
  p75: number | null;
  samples: number;
  rating: "good" | "needs_improvement" | "poor" | null;
}

/** The service's web view (`PA/src/web/query.ts`). */
export interface WebOverview {
  range: { from: string; to: string };
  totals: { visitors: number; views: number; sessions: number; bounce_rate: number | null };
  series: { day: string; visitors: number; views: number }[];
  pages: WebRow[];
  referrers: WebRow[];
  utm: { source: WebRow[]; medium: WebRow[]; campaign: WebRow[] };
  vitals: Record<"LCP" | "INP" | "CLS" | "FCP" | "TTFB", WebVital>;
}

export interface ProductAnalyticsClient {
  /** Public host the app's SDK sends to (`*_ANALYTICS_HOST`). */
  host: string;
  firstEvent(orgId: string, appId: string): Promise<ProductFirstEvent | null>;
  web(orgId: string, appId: string, days: number): Promise<WebOverview>;
  /** `ry_pak_...` for server-side forwarders of one app; never shown in a page or a log. */
  serverKey(appId: string): Promise<string>;
}

export function productAnalyticsClient(cfg: PaConfig, publicHost: string, fetchImpl: typeof fetch = fetch, now: () => Date = () => new Date()): ProductAnalyticsClient {
  const scope = (orgId: string, appId: string) => ({ org_id: orgId, app_ids: [appId], role: "viewer" as const });
  return {
    host: publicHost.replace(/\/$/, ""),
    async firstEvent(orgId, appId) {
      const s = await paFetch<{ first_event: { event: string; at: string } | null }>(cfg, scope(orgId, appId), `/v1/projects/${appId}/status`, {}, fetchImpl, now());
      return s.first_event ? { event: s.first_event.event, at: s.first_event.at } : null;
    },
    web(orgId, appId, days) {
      return paFetch<WebOverview>(cfg, scope(orgId, appId), `/v1/projects/${appId}/web?days=${days}`, {}, fetchImpl, now());
    },
    async serverKey(appId) {
      return `ry_pak_${hex(await hmacSha256(cfg.secret, `pa-server-key:v1:${appId}`))}`;
    },
  };
}

/** Null when the service is not connected on this deployment. */
export function productAnalyticsFromEnv(env: Record<string, string | undefined> = process.env): ProductAnalyticsClient | null {
  const cfg = paConfig(env);
  if (!cfg) return null;
  const host = env.PRODUCT_ANALYTICS_PUBLIC_URL?.trim();
  return productAnalyticsClient(cfg, host && /^https?:\/\//.test(host) ? host : DEFAULT_PA_PUBLIC_HOST);
}
