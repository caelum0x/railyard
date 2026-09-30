import { fmtAt, problemText, type FunnelPerson, type Query } from "../../_lib/insights-client.ts";
import { appQuery, insightsContext } from "../_parts/context.ts";
import { InsightsShell, NO_APP, NOT_CONNECTED, Notice } from "../_parts/Nav.tsx";

export const dynamic = "force-dynamic";

// Who dropped off before a funnel step (up to 100, latest first), each
// linking to their person page.
export default async function FunnelPeoplePage({ searchParams }: { searchParams: { app?: string; q?: string; step?: string } }) {
  const ctx = await insightsContext(searchParams.app);
  const step = Number(searchParams.step ?? "2");
  let people: FunnelPerson[] = [];
  let problem: string | null = null;
  let query: Query | null = null;
  try {
    query = JSON.parse(searchParams.q ?? "null") as Query | null;
  } catch {
    query = null;
  }
  if (ctx.api && query) {
    try {
      ({ people } = await ctx.api.funnelPeople(query, step, true));
    } catch (e) {
      problem = problemText(e);
    }
  }
  const q = appQuery(ctx.app);
  return (
    <InsightsShell title={`Dropped off before step ${step}`} names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/insights">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {!query ? <Notice text="Open this list from a funnel's step." /> : null}
      {problem ? <Notice text={problem} /> : null}
      {query && !problem && ctx.api ? (
        <section className="card">
          {people.length === 0 ? <p className="muted">Nobody dropped off before this step.</p> : null}
          <ul className="list">
            {people.map((p) => (
              <li key={p.id}>
                {p.distinct_id ? <a href={`/product-analytics/persons/${encodeURIComponent(p.distinct_id)}?${q}`}>{String(p.properties?.email ?? p.distinct_id)}</a> : <span>{p.id}</span>}
                <span className="muted">entered {fmtAt(p.entered_at)}</span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </InsightsShell>
  );
}
