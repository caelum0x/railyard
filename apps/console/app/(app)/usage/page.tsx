import { currentSession } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { access } from "../../../lib/ladder.ts";
import { capMessage } from "../../../lib/caps.ts";

export default async function UsagePage() {
  const { repo, insights } = deps();
  const s = (await currentSession())!;
  const [apps, deploys, ent] = await Promise.all([repo.listApps(s.org), repo.listDeploys(s.org), repo.getEntitlement(s.org)]);
  const a = access(ent, new Date());
  const summary = insights ? await insights.summary(s.org, { days: 7 }).catch(() => null) : null;
  const monthEvents = summary ? summary.apps.reduce((sum, app) => sum + (app.month_events ?? 0), 0) : null;
  const now = new Date();
  const capNote = monthEvents === null ? null : capMessage(monthEvents, a.eventsLimit, now);
  return (
    <>
      <h1>Usage</h1>
      <section className="card">
        <h2>Apps</h2>
        {/* copy-source: railyard.apps and railyard.entitlements (live query) */}
        <p data-copy-source="railyard.apps">{apps.length} of {a.appsLimit}</p>
      </section>
      <section className="card">
        <h2>Deploys</h2>
        <p data-copy-source="railyard.deploys">{deploys.length}</p>
      </section>
      <section className="card">
        <h2>Events this month</h2>
        {monthEvents === null ? (
          <p className="muted">No data yet. Events appear here once your app sends its first one.</p>
        ) : (
          <>
            <p data-copy-source="insights /api/v1/tenants/{org}/summary (live query)">
              {monthEvents.toLocaleString("en-US")} of {a.eventsLimit.toLocaleString("en-US")}
            </p>
            {capNote ? <p data-copy-source="lib/caps.ts with the plan's events limit">{capNote}</p> : null}
          </>
        )}
      </section>
    </>
  );
}
