import { currentSession } from "../../../../lib/current.ts";
import { deps } from "../../../../lib/deps.ts";
import type { WebOverview, WebRow } from "../../../../lib/product-analytics.ts";
import { Shell } from "../_components/Shell.tsx";
import { paConfig, paFetch, PaError, pickApp } from "../_lib/http.ts";

export const dynamic = "force-dynamic";

const RANGES = [
  { value: 1, label: "Last day" },
  { value: 7, label: "Last 7 days" },
  { value: 30, label: "Last 30 days" },
  { value: 90, label: "Last 90 days" },
];
const VITALS: { key: keyof WebOverview["vitals"]; label: string; unit: string }[] = [
  { key: "LCP", label: "Largest paint", unit: "ms" },
  { key: "INP", label: "Response to input", unit: "ms" },
  { key: "CLS", label: "Layout shift", unit: "" },
  { key: "FCP", label: "First paint", unit: "ms" },
  { key: "TTFB", label: "Server response", unit: "ms" },
];
const RATING = { good: "Good", needs_improvement: "Needs work", poor: "Poor" } as const;

const fmt = (n: number) => n.toLocaleString("en-US");
const label = (v: string) => (v === "$direct" ? "Direct" : v);

function Table({ title, rows, first }: { title: string; rows: WebRow[]; first: string }) {
  return (
    <section className="card">
      <h2>{title}</h2>
      {rows.length === 0 ? (
        <p className="muted">Nothing in this range yet.</p>
      ) : (
        <table>
          <thead>
            <tr>
              <th>{first}</th>
              <th>Visitors</th>
              <th>Views</th>
            </tr>
          </thead>
          <tbody data-copy-source="product-analytics /v1/projects/{id}/web (live query)">
            {rows.map((r) => (
              <tr key={r.value}>
                <td>{label(r.value)}</td>
                <td>{fmt(r.visitors)}</td>
                <td>{fmt(r.views)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}

// Web view (QY1.2, A18): visitors, page views, top pages, where visits came
// from and page speed, from the same events as every other screen here.
export default async function WebPage({ searchParams }: { searchParams: { app?: string; days?: string } }) {
  const s = (await currentSession())!;
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, searchParams.app);
  const names = apps.map((a) => a.name);
  const cfg = paConfig();
  const days = RANGES.some((r) => String(r.value) === searchParams.days) ? Number(searchParams.days) : 7;
  if (!app || !cfg) {
    return (
      <Shell title="Web" apps={names} app={app?.name ?? null}>
        <section className="card">
          <p className="muted">{!app ? "Add an app first. Its visits appear here." : "Product analytics is arriving on this deployment. Page analytics under Insights keeps working meanwhile."}</p>
        </section>
      </Shell>
    );
  }
  let web: WebOverview | null = null;
  let problem: string | null = null;
  try {
    web = await paFetch<WebOverview>(cfg, { org_id: s.org, app_ids: apps.map((a) => a.id) }, `/v1/projects/${app.id}/web?days=${days}`);
  } catch (e) {
    problem = e instanceof PaError ? e.message : "Product analytics could not be reached. Try again shortly.";
  }
  return (
    <Shell title="Web" apps={names} app={app.name} live>
      <section className="card">
        <form method="get" className="row" style={{ alignItems: "end" }}>
          <input type="hidden" name="app" value={app.name} />
          <label>
            Range
            <select name="days" defaultValue={String(days)}>
              {RANGES.map((r) => (
                <option key={r.value} value={r.value}>
                  {r.label}
                </option>
              ))}
            </select>
          </label>
          <button type="submit">Show</button>
        </form>
      </section>
      {problem ? (
        <section className="card">
          <p>{problem}</p>
        </section>
      ) : null}
      {web ? (
        <>
          <section className="card" data-copy-source="product-analytics /v1/projects/{id}/web (live query)">
            <div className="row">
              <p>
                <strong>{fmt(web.totals.visitors)}</strong> visitors
              </p>
              <p>
                <strong>{fmt(web.totals.views)}</strong> page views
              </p>
              <p>
                <strong>{fmt(web.totals.sessions)}</strong> visits
              </p>
              <p>
                <strong>{web.totals.bounce_rate === null ? "-" : `${Math.round(web.totals.bounce_rate * 100)}%`}</strong> left after one page
              </p>
            </div>
            {web.totals.views === 0 ? <p className="muted">No page views in this range. Add the page tag or the SDK from Setup, then open a page of the app.</p> : null}
          </section>
          <section className="card">
            <h2>Page speed (75th percentile)</h2>
            <table>
              <thead>
                <tr>
                  <th>Measure</th>
                  <th>Value</th>
                  <th>Rating</th>
                  <th>Page views measured</th>
                </tr>
              </thead>
              <tbody data-copy-source="product-analytics /v1/projects/{id}/web page speed (live query)">
                {VITALS.map((v) => {
                  const x = web!.vitals[v.key];
                  return (
                    <tr key={v.key}>
                      <td>
                        {v.label} ({v.key})
                      </td>
                      <td>{x.p75 === null ? "-" : `${v.unit ? Math.round(x.p75) : x.p75.toFixed(3)}${v.unit ? ` ${v.unit}` : ""}`}</td>
                      <td>{x.rating ? RATING[x.rating] : "-"}</td>
                      <td>{fmt(x.samples)}</td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </section>
          <Table title="Top pages" rows={web.pages} first="Page" />
          <Table title="Referrers" rows={web.referrers} first="Came from" />
          <Table title="Campaign source" rows={web.utm.source} first="utm_source" />
          <Table title="Campaign medium" rows={web.utm.medium} first="utm_medium" />
          <Table title="Campaign name" rows={web.utm.campaign} first="utm_campaign" />
        </>
      ) : null}
    </Shell>
  );
}
