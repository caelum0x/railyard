import { ResultView } from "../../_components/charts/ResultView.tsx";
import { fmtAt, problemText, type Dashboard } from "../../_lib/insights-client.ts";
import { appQuery, insightsContext } from "../../insights/_parts/context.ts";
import { InsightsShell, NO_APP, NOT_CONNECTED, Notice } from "../../insights/_parts/Nav.tsx";

export const dynamic = "force-dynamic";

// A dashboard: every tile from the cache (fast), each saying when it was
// computed and whether newer data has arrived. One action: Refresh all.
export default async function DashboardPage({ params, searchParams }: { params: { id: string }; searchParams: { app?: string; refresh?: string } }) {
  const ctx = await insightsContext(searchParams.app);
  let dashboard: Dashboard | null = null;
  let problem: string | null = null;
  if (ctx.api) {
    try {
      ({ dashboard } = await ctx.api.dashboard(params.id, searchParams.refresh === "1"));
    } catch (e) {
      problem = problemText(e);
    }
  }
  const q = appQuery(ctx.app);
  return (
    <InsightsShell title={dashboard?.name ?? "Dashboard"} names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/dashboards">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {problem ? <Notice text={problem} /> : null}
      {dashboard ? (
        <>
          <section className="card row">
            <a className="primary" href={`/product-analytics/dashboards/${dashboard.id}?${q}&refresh=1`}>
              Refresh all
            </a>
            <span className="muted" style={{ fontSize: 13 }}>
              Tiles show their last answer right away; Refresh all asks again.
            </span>
          </section>
          {dashboard.tiles.length === 0 ? <Notice text="This dashboard is empty. Open a saved insight and add it here." /> : null}
          {dashboard.tiles.map((t) => (
            <section className="card" key={t.insight.id}>
              <h2>
                <a href={`/product-analytics/insights/${t.insight.id}?${q}`}>{t.insight.name}</a>
              </h2>
              {t.error ? <p className="error">{t.error}</p> : null}
              {t.answer ? (
                <>
                  <ResultView result={t.answer.result} compactView />
                  <p className="muted" style={{ fontSize: 12 }} data-copy-source="product-analytics insights cache (computed_at)">
                    {t.answer.stale ? `Computed ${fmtAt(t.answer.computed_at)}. Newer data has arrived since; refresh to update.` : `Computed ${fmtAt(t.answer.computed_at)}.`}
                  </p>
                </>
              ) : null}
            </section>
          ))}
        </>
      ) : null}
    </InsightsShell>
  );
}
