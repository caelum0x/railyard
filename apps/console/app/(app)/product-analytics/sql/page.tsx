import { problemText, type SqlResult } from "../_lib/insights-client.ts";
import { insightsContext } from "../insights/_parts/context.ts";
import { InsightsShell, NO_APP, NOT_CONNECTED, Notice } from "../insights/_parts/Nav.tsx";

export const dynamic = "force-dynamic";

const EXAMPLE = "select event, count(*) as events, count(distinct person_id) as people\nfrom pa_view.events\nwhere timestamp > now() - interval '7 days'\ngroup by event\norder by events desc\nlimit 20";

function show(v: unknown): string {
  if (v === null || v === undefined) return "";
  return typeof v === "object" ? JSON.stringify(v) : String(v);
}

// SQL over this app's events, people, groups and sessions. One action: Run.
export default async function SqlPage({ searchParams }: { searchParams: { app?: string; sql?: string } }) {
  const ctx = await insightsContext(searchParams.app);
  const sql = (searchParams.sql ?? "").slice(0, 20_000);
  let result: SqlResult | null = null;
  let problem: string | null = null;
  if (ctx.api && sql.trim()) {
    try {
      result = await ctx.api.sql(sql);
    } catch (e) {
      problem = problemText(e);
    }
  }
  return (
    <InsightsShell title="SQL" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/sql">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.app && ctx.api ? (
        <section className="card">
          <p className="muted" style={{ marginTop: 0 }}>
            One SELECT over pa_view.events, pa_view.persons, pa_view.groups and pa_view.sessions. Queries stop after 10 seconds and return up to 10 000 rows.
          </p>
          <form method="get" action="/product-analytics/sql" style={{ display: "grid", gap: 10 }}>
            <input type="hidden" name="app" value={ctx.app.name} />
            <textarea name="sql" defaultValue={sql || EXAMPLE} rows={8} aria-label="SQL" spellCheck={false} style={{ font: "13px/1.5 ui-monospace, SFMono-Regular, Menlo, monospace", padding: 12, borderRadius: 12, border: "1px solid var(--line)" }} />
            <div>
              <button className="primary" type="submit">
                Run
              </button>
            </div>
          </form>
          {problem ? <p className="error">{problem}</p> : null}
        </section>
      ) : null}
      {result && ctx.app ? (
        <section className="card">
          <div className="row" style={{ justifyContent: "space-between" }}>
            <span className="muted" data-copy-source="product-analytics SQL (live query)">
              {result.rows.length.toLocaleString("en-US")} rows{result.truncated ? " (stopped at the row limit)" : ""}
            </span>
            <form method="post" action="/product-analytics/sql/download">
              <input type="hidden" name="app" value={ctx.app.name} />
              <input type="hidden" name="sql" value={sql} />
              <button className="ghost" type="submit">
                Download CSV
              </button>
            </form>
          </div>
          <div style={{ overflowX: "auto", maxHeight: 520 }}>
            <table style={{ fontSize: 13, borderCollapse: "collapse", fontVariantNumeric: "tabular-nums" }}>
              <thead>
                <tr>
                  {result.columns.map((c, i) => (
                    <th key={`${c}-${i}`} style={{ textAlign: "left", padding: "4px 12px 4px 0" }}>
                      {c}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {result.rows.slice(0, 500).map((r, i) => (
                  <tr key={i}>
                    {r.map((v, j) => (
                      <td key={j} style={{ padding: "3px 12px 3px 0", borderTop: "1px solid var(--line)", maxWidth: 360, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                        {show(v)}
                      </td>
                    ))}
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          {result.rows.length > 500 ? <p className="muted">The page shows the first 500 rows; the CSV has all of them.</p> : null}
        </section>
      ) : null}
    </InsightsShell>
  );
}
