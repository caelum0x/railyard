import { a3Context, appQuery } from "../replay/_parts/context.ts";
import { A3Shell, fmt, NO_APP, NOT_CONNECTED, Notice, Problem } from "../replay/_parts/A3Shell.tsx";
import { KIND_TEXT, problemText, STATUS_TEXT, type Destination } from "../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

// Where this app's events are sent as they arrive (signed webhooks, S3
// buckets), and CSV downloads of saved insights. One action: add a destination.
export default async function ExportsPage({ searchParams }: { searchParams: { app?: string; problem?: string } }) {
  const ctx = await a3Context(searchParams.app);
  const q = appQuery(ctx.app);
  let list: Destination[] = [];
  let max = 0;
  let saved: { id: string; name: string }[] = [];
  let problem: string | null = searchParams.problem ?? null;
  if (ctx.api && ctx.insights) {
    try {
      const [d, s] = await Promise.all([ctx.api.destinations(), ctx.insights.saved()]);
      list = d.destinations;
      max = d.max_destinations;
      saved = s.insights.map((i) => ({ id: i.id, name: i.name }));
    } catch (e) {
      problem = problem ?? problemText(e);
    }
  }
  return (
    <A3Shell title="Exports" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/exports">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.api ? (
        <>
          <section className="card">
            {list.length < max ? (
              <p>
                <a className="primary" href={`/product-analytics/exports/new?${q}`}>
                  Add a destination
                </a>
              </p>
            ) : null}
            <p className="muted" data-copy-source="product-analytics src/exports/destinations.ts MAX_DESTINATIONS">
              New events go to each destination in order, about a minute after they arrive. An app can have {max} destinations.
            </p>
            <Problem text={problem} />
            {list.length ? (
              <table>
                <thead>
                  <tr>
                    <th>Destination</th>
                    <th>Kind</th>
                    <th>State</th>
                    <th>Events sent</th>
                    <th>Last delivery</th>
                  </tr>
                </thead>
                <tbody data-copy-source="product-analytics /exports (live)">
                  {list.map((d) => (
                    <tr key={d.id}>
                      <td>
                        <a href={`/product-analytics/exports/${d.id}?${q}`}>{d.name}</a>
                      </td>
                      <td>{KIND_TEXT[d.kind]}</td>
                      <td>{d.enabled ? STATUS_TEXT[d.status] : "Paused"}</td>
                      <td>{d.delivered.toLocaleString("en-US")}</td>
                      <td>{fmt(d.last_success_at)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            ) : (
              <p className="muted">No destinations yet.</p>
            )}
          </section>
          <section className="card">
            <h2>Download an insight as CSV</h2>
            {saved.length ? (
              <form method="get" action="/product-analytics/exports/csv" className="row" style={{ alignItems: "end" }}>
                <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
                <select name="insight">
                  {saved.map((s) => (
                    <option key={s.id} value={s.id}>
                      {s.name}
                    </option>
                  ))}
                </select>
                <button type="submit">Download CSV</button>
              </form>
            ) : (
              <p className="muted">Save an insight first; it can then be downloaded here. The SQL screen downloads its own answers.</p>
            )}
          </section>
        </>
      ) : null}
    </A3Shell>
  );
}
