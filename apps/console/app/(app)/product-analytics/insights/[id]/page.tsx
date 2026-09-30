import { redirect } from "next/navigation";
import { ResultView } from "../../_components/charts/ResultView.tsx";
import { builderHref, fmtAt, KINDS, problemText, type Answer, type DashboardSummary, type SavedInsight } from "../../_lib/insights-client.ts";
import { appQuery, insightsContext } from "../_parts/context.ts";
import { InsightsShell, NO_APP, NOT_CONNECTED, Notice } from "../_parts/Nav.tsx";

export const dynamic = "force-dynamic";

async function addToDashboard(form: FormData) {
  "use server";
  const ctx = await insightsContext(String(form.get("app") ?? ""));
  if (!ctx.api || !ctx.app) redirect("/product-analytics/dashboards");
  const dashboard = String(form.get("dashboard") ?? "");
  await ctx.api.addTile(dashboard, String(form.get("insight") ?? ""));
  redirect(`/product-analytics/dashboards/${encodeURIComponent(dashboard)}?${appQuery(ctx.app)}`);
}

async function deleteInsight(form: FormData) {
  "use server";
  const ctx = await insightsContext(String(form.get("app") ?? ""));
  if (ctx.api) await ctx.api.deleteSaved(String(form.get("insight") ?? ""));
  redirect(`/product-analytics/insights?${appQuery(ctx.app)}`);
}

// One saved insight. The one action is Refresh; a cached answer shows when
// it was computed, and says so when newer data has arrived since.
export default async function SavedInsightPage({ params, searchParams }: { params: { id: string }; searchParams: { app?: string; refresh?: string } }) {
  const ctx = await insightsContext(searchParams.app);
  let insight: SavedInsight | null = null;
  let answer: Answer | null = null;
  let dashboards: DashboardSummary[] = [];
  let problem: string | null = null;
  if (ctx.api) {
    try {
      ({ insight } = await ctx.api.savedOne(params.id));
      [answer, { dashboards }] = await Promise.all([ctx.api.query(insight.query, searchParams.refresh === "1"), ctx.api.dashboards()]);
    } catch (e) {
      problem = problemText(e);
    }
  }
  const q = appQuery(ctx.app);
  return (
    <InsightsShell title={insight?.name ?? "Insight"} names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/insights">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {problem ? <Notice text={problem} /> : null}
      {insight && answer && ctx.app ? (
        <>
          <section className="card">
            <p className="muted" style={{ marginTop: 0 }}>
              {KINDS.find((k) => k.kind === insight.kind)?.label}
            </p>
            <ResultView result={answer.result} dropOffBase={`/product-analytics/insights/people?${q}&q=${encodeURIComponent(JSON.stringify(insight.query))}`} />
            <div className="row" style={{ marginTop: 12 }}>
              <span className="muted" style={{ fontSize: 13 }} data-copy-source="product-analytics insights cache (computed_at)">
                {answer.stale ? `Computed ${fmtAt(answer.computed_at)}. Newer data has arrived since.` : `Computed ${fmtAt(answer.computed_at)}.`}
              </span>
              <a className="primary" href={`/product-analytics/insights/${insight.id}?${q}&refresh=1`}>
                Refresh
              </a>
              <a className="ghost" href={builderHref(insight.query, ctx.app.name)}>
                Change
              </a>
            </div>
          </section>
          <section className="card">
            <h2>Dashboards</h2>
            {dashboards.length ? (
              <form action={addToDashboard} className="row">
                <input type="hidden" name="app" value={ctx.app.name} />
                <input type="hidden" name="insight" value={insight.id} />
                <select name="dashboard" aria-label="Dashboard">
                  {dashboards.map((d) => (
                    <option key={d.id} value={d.id}>
                      {d.name}
                    </option>
                  ))}
                </select>
                <button className="ghost" type="submit">
                  Add to dashboard
                </button>
              </form>
            ) : (
              <p className="muted">
                No dashboards yet. <a href={`/product-analytics/dashboards?${q}`}>Create one</a> to pin this insight.
              </p>
            )}
            <form action={deleteInsight} style={{ marginTop: 12 }}>
              <input type="hidden" name="app" value={ctx.app.name} />
              <input type="hidden" name="insight" value={insight.id} />
              <button className="ghost" type="submit">
                Delete this insight
              </button>
            </form>
          </section>
        </>
      ) : null}
    </InsightsShell>
  );
}
