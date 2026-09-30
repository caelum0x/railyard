import { redirect } from "next/navigation";
import { problemText, type DashboardSummary } from "../_lib/insights-client.ts";
import { appQuery, insightsContext } from "../insights/_parts/context.ts";
import { InsightsShell, NO_APP, NOT_CONNECTED, Notice } from "../insights/_parts/Nav.tsx";

export const dynamic = "force-dynamic";

async function create(form: FormData) {
  "use server";
  const ctx = await insightsContext(String(form.get("app") ?? ""));
  if (!ctx.api || !ctx.app) redirect("/product-analytics/dashboards");
  const { dashboard } = await ctx.api.createDashboard(String(form.get("name") ?? ""));
  redirect(`/product-analytics/dashboards/${dashboard.id}?${appQuery(ctx.app)}`);
}

// Dashboards for one app. One action: create a dashboard.
export default async function DashboardsPage({ searchParams }: { searchParams: { app?: string } }) {
  const ctx = await insightsContext(searchParams.app);
  let dashboards: DashboardSummary[] = [];
  let problem: string | null = null;
  if (ctx.api) {
    try {
      ({ dashboards } = await ctx.api.dashboards());
    } catch (e) {
      problem = problemText(e);
    }
  }
  const q = appQuery(ctx.app);
  return (
    <InsightsShell title="Dashboards" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/dashboards">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.app && ctx.api ? (
        <>
          <section className="card">
            <form action={create} className="row">
              <input type="hidden" name="app" value={ctx.app.name} />
              <input type="text" name="name" placeholder="Weekly review" aria-label="Dashboard name" required maxLength={200} />
              <button className="primary" type="submit">
                Create dashboard
              </button>
            </form>
          </section>
          <section className="card">
            {problem ? <p>{problem}</p> : null}
            {!problem && dashboards.length === 0 ? <p className="muted">No dashboards yet. Create one, then add saved insights from their pages.</p> : null}
            <ul className="list">
              {dashboards.map((d) => (
                <li key={d.id}>
                  <a href={`/product-analytics/dashboards/${d.id}?${q}`}>{d.name}</a>
                  <span className="muted" data-copy-source="product-analytics dashboards (live)">
                    {d.tiles} {d.tiles === 1 ? "insight" : "insights"}
                  </span>
                </li>
              ))}
            </ul>
          </section>
        </>
      ) : null}
    </InsightsShell>
  );
}
