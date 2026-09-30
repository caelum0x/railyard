// Client for Railyard Insights' tenant API (the fork's
// /api/v1/tenants/{org}/summary, bearer INSIGHTS_API_TOKEN). Server-only.

export interface InsightsSummary {
  org_id: string;
  since: string;
  apps: { app_id: string; first_event_at: string | null; events: number; visitors: number; month_events?: number }[];
  funnel: { steps: { name: "visit" | "signup" | "first_action"; sessions: number }[] };
}

export type VitalMetric = "lcp" | "inp" | "cls" | "fcp" | "ttfb";
export type VitalRating = "good" | "needs-improvement" | "poor";
export type VitalsRow = { app_id: string; samples: number } & Record<VitalMetric, { p75: number | null; rating: VitalRating | null }>;

/** The fork's `/api/v1/tenants/{org}/vitals` answer (PY3.3). */
export interface InsightsVitals {
  since: string;
  apps: VitalsRow[];
  daily: (VitalsRow & { day: string })[];
  deploys: { at: string; before: VitalsRow | null; after: VitalsRow | null }[];
}

export interface InsightsClient {
  summary(orgId: string, opts?: { app?: string; days?: number }): Promise<InsightsSummary>;
  /** Web vitals p75 (PY3.3); optional so older fakes and deployments still type-check. */
  vitals?(orgId: string, opts: { app: string; days?: number; marks?: string[] }): Promise<InsightsVitals>;
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function isSummary(v: unknown): v is InsightsSummary {
  const s = v as InsightsSummary;
  return (
    !!s &&
    typeof s.org_id === "string" &&
    Array.isArray(s.apps) &&
    s.apps.every((a) => typeof a.app_id === "string" && typeof a.events === "number" && typeof a.visitors === "number") &&
    Array.isArray(s.funnel?.steps) &&
    s.funnel.steps.length === 3
  );
}

export function insightsClient(opts: { apiUrl: string; token: string; fetchImpl?: typeof fetch }): InsightsClient {
  const base = opts.apiUrl.replace(/\/$/, "");
  const fetchImpl = opts.fetchImpl ?? fetch;
  return {
    async summary(orgId, { app, days = 7 } = {}) {
      if (!UUID.test(orgId) || (app !== undefined && !UUID.test(app))) throw new Error("invalid org or app id");
      const q = new URLSearchParams({ days: String(days), ...(app ? { app } : {}) });
      const res = await fetchImpl(`${base}/api/v1/tenants/${orgId}/summary?${q}`, { headers: { authorization: `Bearer ${opts.token}`, accept: "application/json" } });
      if (!res.ok) throw new Error(`Insights answered ${res.status}`);
      const body = await res.json();
      if (!isSummary(body)) throw new Error("Insights answered with an unexpected shape");
      return body;
    },
    async vitals(orgId, { app, days = 7, marks = [] }) {
      if (!UUID.test(orgId) || !UUID.test(app)) throw new Error("invalid org or app id");
      const q = new URLSearchParams({ app, days: String(days), ...(marks.length ? { marks: marks.slice(0, 10).join(",") } : {}) });
      const res = await fetchImpl(`${base}/api/v1/tenants/${orgId}/vitals?${q}`, { headers: { authorization: `Bearer ${opts.token}`, accept: "application/json" } });
      if (!res.ok) throw new Error(`Insights answered ${res.status}`);
      const body = (await res.json()) as InsightsVitals;
      if (!body || !Array.isArray(body.apps) || !Array.isArray(body.daily) || !Array.isArray(body.deploys)) throw new Error("Insights answered with an unexpected shape");
      return body;
    },
  };
}

/** The earliest first event across the org's apps (the analytics proof), or null. */
export function firstEvent(summary: InsightsSummary, appId?: string): { app_id: string; at: string } | null {
  const hits = summary.apps.filter((a) => a.first_event_at && (!appId || a.app_id === appId));
  hits.sort((a, b) => (a.first_event_at! < b.first_event_at! ? -1 : 1));
  return hits[0] ? { app_id: hits[0].app_id, at: hits[0].first_event_at! } : null;
}
