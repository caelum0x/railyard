import { currentSession } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { Shell } from "./_components/Shell.tsx";
import { fmtTime, paConfig, paFetch, PaError, pickApp, type FeedEvent, type Status } from "./_lib/http.ts";

export const dynamic = "force-dynamic";

// Events screen: the first-event proof and the live feed of the latest
// events for one app (refreshed every 10 seconds).
export default async function ProductAnalyticsPage({ searchParams }: { searchParams: { app?: string } }) {
  const s = (await currentSession())!;
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, searchParams.app);
  const names = apps.map((a) => a.name);
  const cfg = paConfig();
  if (!app) {
    return (
      <Shell title="Product analytics" apps={names} app={null}>
        <section className="card">
          <p className="muted">Add an app first. Its events appear here.</p>
          <a href="/apps">Add an app</a>
        </section>
      </Shell>
    );
  }
  if (!cfg) {
    return (
      <Shell title="Product analytics" apps={names} app={app.name}>
        <section className="card">
          <p className="muted">Product analytics is arriving on this deployment. Page analytics under Insights keeps working meanwhile.</p>
        </section>
      </Shell>
    );
  }
  const scope = { org_id: s.org, app_ids: apps.map((a) => a.id) };
  let status: Status | null = null;
  let feed: FeedEvent[] = [];
  let problem: string | null = null;
  try {
    [status, { events: feed }] = await Promise.all([
      paFetch<Status>(cfg, scope, `/v1/projects/${app.id}/status`),
      paFetch<{ events: FeedEvent[] }>(cfg, scope, `/v1/projects/${app.id}/events?limit=50`),
    ]);
  } catch (e) {
    problem = e instanceof PaError ? e.message : "Product analytics could not be reached. Try again shortly.";
  }
  return (
    <Shell title="Product analytics" apps={names} app={app.name} live>
      {problem ? (
        <section className="card">
          <p>{problem}</p>
        </section>
      ) : null}
      {status ? (
        <section className="card">
          <h2>First event</h2>
          {status.first_event ? (
            <p data-copy-source="product-analytics /v1/projects/{id}/status (live query)">
              Received {status.first_event.event} at {fmtTime(status.first_event.at)}.
            </p>
          ) : (
            <>
              <p>Waiting for the first event from {app.name}.</p>
              <a href={`/product-analytics/setup?app=${encodeURIComponent(app.name)}`}>Set up the app</a>
            </>
          )}
          {status.paused ? <p className="muted">This organization is paused, so new events are not stored.</p> : null}
          {status.cap_message ? <p data-copy-source="lib/caps.ts with the plan's events limit">{status.cap_message}</p> : null}
          <p className="muted" data-copy-source="product-analytics /v1/projects/{id}/status (live query)">
            This month: {status.usage.stored.toLocaleString("en-US")} stored, {status.usage.capped.toLocaleString("en-US")} counted over the limit.
          </p>
        </section>
      ) : null}
      {status ? (
        <section className="card">
          <h2>Latest events</h2>
          {feed.length === 0 ? (
            <p className="muted">No events yet. They appear here within seconds of arriving.</p>
          ) : (
            <table>
              <thead>
                <tr>
                  <th>Time</th>
                  <th>Event</th>
                  <th>Person</th>
                  <th>Where</th>
                </tr>
              </thead>
              <tbody data-copy-source="product-analytics /v1/projects/{id}/events (live query)">
                {feed.map((e) => (
                  <tr key={e.uuid}>
                    <td>{fmtTime(e.timestamp)}</td>
                    <td>{e.event}</td>
                    <td>
                      <a href={`/product-analytics/persons/${encodeURIComponent(e.distinct_id)}?app=${encodeURIComponent(app.name)}`}>{e.distinct_id}</a>
                    </td>
                    <td>{e.screen ?? e.url ?? ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </section>
      ) : null}
    </Shell>
  );
}
