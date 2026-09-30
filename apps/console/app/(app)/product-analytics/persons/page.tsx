import { currentSession } from "../../../../lib/current.ts";
import { deps } from "../../../../lib/deps.ts";
import { Shell } from "../_components/Shell.tsx";
import { fmtTime, paConfig, paFetch, PaError, pickApp, type PersonRow } from "../_lib/http.ts";

export const dynamic = "force-dynamic";

// People: find one person by id, email or name.
export default async function PersonsPage({ searchParams }: { searchParams: { app?: string; q?: string } }) {
  const s = (await currentSession())!;
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, searchParams.app);
  const cfg = paConfig();
  const q = (searchParams.q ?? "").slice(0, 200);
  let persons: PersonRow[] = [];
  let problem: string | null = null;
  if (app && cfg) {
    try {
      ({ persons } = await paFetch<{ persons: PersonRow[] }>(cfg, { org_id: s.org, app_ids: apps.map((a) => a.id) }, `/v1/projects/${app.id}/persons?limit=50&search=${encodeURIComponent(q)}`));
    } catch (e) {
      problem = e instanceof PaError ? e.message : "Product analytics could not be reached. Try again shortly.";
    }
  }
  return (
    <Shell title="People" apps={apps.map((a) => a.name)} app={app?.name ?? null}>
      <section className="card">
        <form method="get" className="row">
          {app ? <input type="hidden" name="app" value={app.name} /> : null}
          <input name="q" defaultValue={q} placeholder="Id, email or name" aria-label="Find a person" />
          <button type="submit">Find</button>
        </form>
      </section>
      <section className="card">
        {!app ? <p className="muted">Add an app first.</p> : null}
        {app && !cfg ? <p className="muted">Product analytics is arriving on this deployment.</p> : null}
        {problem ? <p>{problem}</p> : null}
        {app && cfg && !problem && persons.length === 0 ? <p className="muted">{q ? "Nobody matches that." : "No people yet. A person appears with their first event."}</p> : null}
        {persons.length ? (
          <table>
            <thead>
              <tr>
                <th>Person</th>
                <th>Email</th>
                <th>Signed in</th>
                <th>First seen</th>
              </tr>
            </thead>
            <tbody data-copy-source="product-analytics /v1/projects/{id}/persons (live query)">
              {persons.map((p) => {
                const id = p.distinct_ids?.[0] ?? p.id;
                return (
                  <tr key={p.id}>
                    <td>
                      <a href={`/product-analytics/persons/${encodeURIComponent(id)}?app=${encodeURIComponent(app!.name)}`}>{id}</a>
                    </td>
                    <td>{typeof p.properties.email === "string" ? p.properties.email : ""}</td>
                    <td>{p.is_identified ? "Yes" : "No"}</td>
                    <td>{fmtTime(p.created_at)}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        ) : null}
      </section>
    </Shell>
  );
}
