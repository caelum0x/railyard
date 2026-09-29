import { currentSession } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { access } from "../../../lib/ladder.ts";

export default async function UsagePage() {
  const { repo } = deps();
  const s = (await currentSession())!;
  const [apps, deploys, ent] = await Promise.all([repo.listApps(s.org), repo.listDeploys(s.org), repo.getEntitlement(s.org)]);
  const a = access(ent, new Date());
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
        <p className="muted">No data yet. Events appear here once your app sends its first one.</p>
      </section>
    </>
  );
}
