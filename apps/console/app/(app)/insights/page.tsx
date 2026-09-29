import { currentSession } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { insightsView } from "../../../lib/insights-view.ts";
import { access } from "../../../lib/ladder.ts";

// Visitors, events and the first funnel (visit, sign-up, first action) for
// the org's apps over the last 7 days, read live from Insights (RYP-E1, PY1.3).
export default async function InsightsPage() {
  const { repo, insights } = deps();
  const s = (await currentSession())!;
  const apps = await repo.listApps(s.org);
  const summary = insights ? await insights.summary(s.org, { days: 7 }).catch(() => null) : null;
  if (!summary) {
    return (
      <>
        <h1>Insights</h1>
        <section className="card">
          <p className="muted">No data yet. Events appear here once your app sends its first one.</p>
        </section>
      </>
    );
  }
  const limit = access(await repo.getEntitlement(s.org), new Date()).eventsLimit;
  const view = insightsView(summary, new Map(apps.map((a) => [a.id, a.name])), limit);
  return (
    <>
      <h1>Insights</h1>
      <section className="card">
        <h2>Last 7 days</h2>
        {view.empty ? <p className="muted">No events yet. Add the page tag from the Apps screen and open your site once.</p> : null}
        {view.capNote ? <p data-copy-source="lib/caps.ts with the plan's events limit">{view.capNote}</p> : null}
        <table>
          <thead>
            <tr>
              <th>App</th>
              <th>Visitors</th>
              <th>Events</th>
              <th>First event</th>
            </tr>
          </thead>
          <tbody data-copy-source="insights /api/v1/tenants/{org}/summary (live query)">
            {view.apps.map((a) => (
              <tr key={a.name}>
                <td>{a.name}</td>
                <td>{a.visitors}</td>
                <td>{a.events}</td>
                <td>{a.firstEvent ?? "Waiting"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
      <section className="card">
        <h2>From visit to first action</h2>
        <ol data-copy-source="insights /api/v1/tenants/{org}/summary (live query)">
          {view.steps.map((st) => (
            <li key={st.label}>
              {st.label}: {st.sessions}
              {st.ofPrevious ? ` (${st.ofPrevious} of the step before)` : ""}
            </li>
          ))}
        </ol>
        <p className="muted">A sign-up is an event named signup; a first action is any other event after it.</p>
      </section>
    </>
  );
}
