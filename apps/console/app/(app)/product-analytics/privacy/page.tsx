import { currentSession } from "../../../../lib/current.ts";
import { deps } from "../../../../lib/deps.ts";
import { Shell } from "../_components/Shell.tsx";
import { fmtTime, paConfig, paFetch, PaError, pickApp, type PrivacyJob, type Status } from "../_lib/http.ts";

export const dynamic = "force-dynamic";

const STATUS_WORDS: Record<PrivacyJob["status"], string> = {
  queued: "Waiting",
  running: "Deleting now",
  done: "Deleted",
  failed: "Could not finish; retry from the person's page",
};

// Privacy: what is kept, for how long, and every deletion request.
export default async function PrivacyPage({ searchParams }: { searchParams: { app?: string } }) {
  const s = (await currentSession())!;
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, searchParams.app);
  const cfg = paConfig();
  let jobs: PrivacyJob[] = [];
  let status: Status | null = null;
  let problem: string | null = null;
  if (app && cfg) {
    const scope = { org_id: s.org, app_ids: apps.map((a) => a.id) };
    try {
      [{ jobs }, status] = await Promise.all([
        paFetch<{ jobs: PrivacyJob[] }>(cfg, scope, `/v1/projects/${app.id}/privacy/jobs`),
        paFetch<Status>(cfg, scope, `/v1/projects/${app.id}/status`),
      ]);
    } catch (e) {
      problem = e instanceof PaError ? e.message : "Product analytics could not be reached. Try again shortly.";
    }
  }
  const pending = jobs.some((j) => j.status === "queued" || j.status === "running");
  return (
    <Shell title="Privacy" apps={apps.map((a) => a.name)} app={app?.name ?? null} live={pending}>
      <section className="card">
        <h2>What is kept</h2>
        <ul>
          <li>No IP address is stored or logged. The country comes from the network edge.</li>
          <li>Nothing is sent from a visitor&apos;s device before they agree, unless you turn that off in Setup.</li>
          {status ? <li data-copy-source="apps/product-analytics/src/plans.ts">Events are kept {status.retention_days} days, then removed.</li> : null}
          <li>A person can be deleted or exported from their page under People.</li>
        </ul>
      </section>
      <section className="card">
        <h2>Deletion requests</h2>
        {problem ? <p>{problem}</p> : null}
        {!problem && jobs.length === 0 ? <p className="muted">No requests yet.</p> : null}
        {jobs.length ? (
          <table>
            <thead>
              <tr>
                <th>Person</th>
                <th>Asked</th>
                <th>Due by</th>
                <th>Status</th>
                <th>Finished</th>
              </tr>
            </thead>
            <tbody data-copy-source="product-analytics /v1/projects/{id}/privacy/jobs (live query)">
              {jobs.map((j) => (
                <tr key={j.id}>
                  <td>{j.distinct_id}</td>
                  <td>{fmtTime(j.requested_at)}</td>
                  <td>{fmtTime(j.due_at)}</td>
                  <td>{STATUS_WORDS[j.status]}</td>
                  <td>{j.finished_at ? `${fmtTime(j.finished_at)} (${j.result?.events ?? 0} events)` : ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
        ) : null}
      </section>
    </Shell>
  );
}
