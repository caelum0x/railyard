import { currentSession } from "../../../../lib/current.ts";
import { deps } from "../../../../lib/deps.ts";
import { markTimes, vitalsView, type VitalsView } from "../../../../lib/vitals-view.ts";

export const dynamic = "force-dynamic";

// Page speed per app over 7 days, with a marker for each deploy and what
// changed after it (Wave P PY3.3). Shown only with RAILYARD_REPLAY_LIVE=1.
export default async function VitalsPage() {
  const { repo, insights, cfg } = deps();
  if (!cfg.replayLive || !insights?.vitals) {
    return (
      <>
        <h1>Page speed</h1>
        <section className="card">
          <p className="muted">This part of Insights is arriving; it is not switched on for this console yet.</p>
        </section>
      </>
    );
  }
  const s = (await currentSession())!;
  const now = new Date();
  const since = new Date(now.getTime() - 7 * 86_400_000);
  const apps = await repo.listApps(s.org);
  const views: { name: string; view: VitalsView | null }[] = [];
  for (const app of apps) {
    const marks = markTimes(await repo.listDeploys(s.org, app.id), app.id, since);
    const v = await insights.vitals(s.org, { app: app.id, days: 7, marks: marks.map((d) => d.created_at) }).catch(() => null);
    views.push({ name: app.name, view: v ? vitalsView(v, app.id, marks) : null });
  }
  return (
    <>
      <h1>Page speed</h1>
      <p className="muted">The 75th percentile of page views in the last 7 days, and how each metric moved from the day before a deploy to the day after it.</p>
      {views.length === 0 ? (
        <section className="card">
          <p className="muted">No apps yet. Add one on the Apps screen.</p>
        </section>
      ) : null}
      {views.map(({ name, view }) => (
        <section className="card" key={name}>
          <h2>{name}</h2>
          {!view ? (
            <p className="muted">Could not read page speed just now; try again in a minute.</p>
          ) : view.empty ? (
            <p className="muted">No page speed yet. It arrives with the next page views once the page tag or the web package is on the site.</p>
          ) : (
            <>
              <table>
                <thead>
                  <tr>
                    <th>Metric</th>
                    <th>75th percentile</th>
                    <th>Rating</th>
                  </tr>
                </thead>
                <tbody data-copy-source="insights /api/v1/tenants/{org}/vitals (live query)">
                  {view.metrics.map((m) => (
                    <tr key={m.key}>
                      <td>{m.label}</td>
                      <td>{m.value}</td>
                      <td>{m.rating ?? ""}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <p className="muted">From {view.samples} page views.</p>
            </>
          )}
          {view && view.deploys.length ? (
            <>
              <h3>Deploys</h3>
              <ul data-copy-source="console deploy records and insights vitals around each deploy">
                {view.deploys.map((d) => (
                  <li key={d.id}>
                    {d.at.slice(0, 16).replace("T", " ")} UTC, {d.branch}: {d.changes.length ? d.changes.join("; ") : "not enough page views on both sides yet"}
                  </li>
                ))}
              </ul>
            </>
          ) : null}
        </section>
      ))}
    </>
  );
}
