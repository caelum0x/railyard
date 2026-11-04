import { currentSession } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { access } from "../../../lib/ladder.ts";
import { capMessage } from "../../../lib/caps.ts";
import { classifyAppEventAnomaly, computeEventAnomalyFeatures, worstEventAnomaly } from "../../../lib/eventAnomaly.ts";

/** Current period (today) vs. a trailing baseline (the last 30 days) -- both real insightsClient.summary() calls, never synthetic. */
const CURRENT_PERIOD_DAYS = 1;
const BASELINE_PERIOD_DAYS = 30;

/** The worst real per-app anomaly across the org, or null on no data/no gateway/no real anomaly. Fail-open: never throws, never blocks the page. */
async function eventAnomalyBadge(
  insights: ReturnType<typeof deps>["insights"],
  eventAnomaly: ReturnType<typeof deps>["eventAnomaly"],
  orgId: string,
): Promise<ReturnType<typeof worstEventAnomaly>> {
  if (!insights || !eventAnomaly) return null;
  const [current, baseline] = await Promise.all([
    insights.summary(orgId, { days: CURRENT_PERIOD_DAYS }).catch(() => null),
    insights.summary(orgId, { days: BASELINE_PERIOD_DAYS }).catch(() => null),
  ]);
  if (!current || !baseline) return null;
  const features = computeEventAnomalyFeatures(current, baseline, BASELINE_PERIOD_DAYS);
  const results = await Promise.all(features.map((f) => classifyAppEventAnomaly(eventAnomaly, f, CURRENT_PERIOD_DAYS, BASELINE_PERIOD_DAYS)));
  return worstEventAnomaly(results);
}

export default async function UsagePage() {
  const { repo, insights, eventAnomaly } = deps();
  const s = (await currentSession())!;
  const [apps, deploys, ent] = await Promise.all([repo.listApps(s.org), repo.listDeploys(s.org), repo.getEntitlement(s.org)]);
  const a = access(ent, new Date());
  const summary = insights ? await insights.summary(s.org, { days: 7 }).catch(() => null) : null;
  const monthEvents = summary ? summary.apps.reduce((sum, app) => sum + (app.month_events ?? 0), 0) : null;
  const now = new Date();
  const capNote = monthEvents === null ? null : capMessage(monthEvents, a.eventsLimit, now);
  const anomaly = await eventAnomalyBadge(insights, eventAnomaly, s.org);
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
            {anomaly ? (
              <p data-copy-source="lib/eventAnomaly.ts (Evren /v1/systemone, grounded against the real event ratio)">
                {anomaly.category === "traffic_spike" ? "Unusual activity: traffic spike. " : "Unusual activity: traffic drop. "}
                {anomaly.reason}
              </p>
            ) : null}
          </>
        )}
      </section>
    </>
  );
}
