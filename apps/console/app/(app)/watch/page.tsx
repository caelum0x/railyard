import { WatchPanel } from "../../../components/WatchPanel.tsx";
import { currentSession } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { listWithState } from "../../../lib/watch/service.ts";

// Uptime checks and status pages (Wave P PY2.2, RYP-E4). Checks run every
// minute; two failures in a row turn a check red and email the owner.
export default async function WatchPage() {
  const { repo, watch } = deps();
  if (!watch.live) {
    return (
      <>
        <h1>Watch</h1>
        <section className="card">
          <p className="muted">Watch is arriving. Nothing to set up yet.</p>
        </section>
      </>
    );
  }
  const s = (await currentSession())!;
  const [apps, checks] = await Promise.all([repo.listApps(s.org), listWithState(watch, s.org)]);
  return (
    <>
      <h1>Watch</h1>
      <section className="card">
        <WatchPanel apps={apps.map((a) => a.name)} checks={checks} />
      </section>
    </>
  );
}
