import { KINDS, problemText, type SavedInsight } from "../_lib/insights-client.ts";
import { appQuery, insightsContext } from "./_parts/context.ts";
import { InsightsShell, NO_APP, NOT_CONNECTED, Notice } from "./_parts/Nav.tsx";

export const dynamic = "force-dynamic";

// Saved insights for one app, newest first. One action: start a new insight.
export default async function InsightsPage({ searchParams }: { searchParams: { app?: string } }) {
  const ctx = await insightsContext(searchParams.app);
  const shell = (children: React.ReactNode) => (
    <InsightsShell title="Insights" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/insights">
      {children}
    </InsightsShell>
  );
  if (!ctx.app) return shell(<Notice text={NO_APP} />);
  if (!ctx.api) return shell(<Notice text={NOT_CONNECTED} />);
  let saved: SavedInsight[] = [];
  let problem: string | null = null;
  try {
    ({ insights: saved } = await ctx.api.saved());
  } catch (e) {
    problem = problemText(e);
  }
  const q = appQuery(ctx.app);
  const kindLabel = (k: string) => KINDS.find((x) => x.kind === k)?.label ?? k;
  return shell(
    <>
      <section className="card">
        <p className="muted">Ask how people use {ctx.app.name}: trends over time, funnels, retention, paths, stickiness and lifecycle.</p>
        <a className="primary" href={`/product-analytics/insights/new?${q}`}>
          New insight
        </a>
      </section>
      <section className="card">
        <h2>Saved insights</h2>
        {problem ? <p>{problem}</p> : null}
        {!problem && saved.length === 0 ? <p className="muted">Nothing saved yet. Save an insight to find it here and put it on a dashboard.</p> : null}
        <ul className="list">
          {saved.map((i) => (
            <li key={i.id}>
              <a href={`/product-analytics/insights/${i.id}?${q}`}>{i.name}</a>
              <span className="muted">{kindLabel(i.kind)}</span>
            </li>
          ))}
        </ul>
      </section>
    </>,
  );
}
