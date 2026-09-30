import { redirect } from "next/navigation";
import { fmtAt, problemText, type Cohort, type Meta } from "../../_lib/insights-client.ts";
import { appQuery, insightsContext } from "../_parts/context.ts";
import { EventSelect, Text } from "../_parts/fields.tsx";
import { InsightsShell, NO_APP, NOT_CONNECTED, Notice } from "../_parts/Nav.tsx";

export const dynamic = "force-dynamic";

const MAX_FILE = 5 * 1024 * 1024;

async function create(form: FormData) {
  "use server";
  const ctx = await insightsContext(String(form.get("app") ?? ""));
  if (!ctx.api || !ctx.app) redirect("/product-analytics/insights/segments");
  const back = `/product-analytics/insights/segments?${appQuery(ctx.app)}`;
  const name = String(form.get("name") ?? "").trim();
  const file = form.get("file");
  try {
    if (file && typeof file === "object" && "size" in file && file.size > 0) {
      if (file.size > MAX_FILE) redirect(`${back}&problem=${encodeURIComponent("Keep the file under 5 MB.")}`);
      const made = (await ctx.api.createCohort({ name, kind: "static" })).segment;
      const up = await ctx.api.uploadCohort(made.id, await file.text());
      redirect(`${back}&made=${encodeURIComponent(made.name)}&unmatched=${up.segment.unmatched}`);
    }
    const criteria: Record<string, unknown>[] = [];
    const event = String(form.get("event") ?? "");
    if (event) {
      const op = String(form.get("op") ?? "gte") === "lte" ? "lte" : "gte";
      criteria.push({
        type: "behaviour",
        ...(event.startsWith("action:") ? { action_id: event.slice(7) } : { event }),
        operator: op,
        count: Number(form.get("count") ?? "1"),
        days: Number(form.get("days") ?? "30"),
      });
    }
    const key = String(form.get("pkey") ?? "").trim();
    if (key) criteria.push({ type: "person", key, operator: "exact", value: String(form.get("pval") ?? "") });
    const made = (await ctx.api.createCohort({ name, kind: "behavioural", definition: { match: "all", criteria } })).segment;
    redirect(`${back}&made=${encodeURIComponent(made.name)}`);
  } catch (e) {
    if (e && typeof e === "object" && "digest" in e) throw e; // redirects
    redirect(`${back}&problem=${encodeURIComponent(problemText(e))}`);
  }
}

// Segments: saved sets of people to filter any insight by. One action:
// create one, either from what people did or from a file of ids or emails.
export default async function SegmentsPage({ searchParams }: { searchParams: { app?: string; made?: string; unmatched?: string; problem?: string } }) {
  const ctx = await insightsContext(searchParams.app);
  let segments: Cohort[] = [];
  let meta: Meta | null = null;
  let problem: string | null = searchParams.problem ?? null;
  if (ctx.api) {
    try {
      [{ segments }, meta] = await Promise.all([ctx.api.cohorts(), ctx.api.meta()]);
    } catch (e) {
      problem = problemText(e);
    }
  }
  return (
    <InsightsShell title="Segments" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/insights/segments">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.app && meta ? (
        <section className="card">
          <form action={create} style={{ display: "grid", gap: 12 }}>
            <input type="hidden" name="app" value={ctx.app.name} />
            <Text name="name" title="Name" placeholder="Repeat buyers" />
            <div className="row" style={{ alignItems: "end" }}>
              <EventSelect name="event" meta={meta} blank="Anything (no condition)" title="People who did" />
              <label style={{ display: "grid", gap: 4, fontSize: 13 }}>
                <span className="muted">How often</span>
                <select name="op" defaultValue="gte">
                  <option value="gte">at least</option>
                  <option value="lte">at most</option>
                </select>
              </label>
              <Text name="count" value="1" title="times" />
              <Text name="days" value="30" title="in the last days" />
            </div>
            <div className="row" style={{ alignItems: "end" }}>
              <Text name="pkey" title="and whose property" placeholder="plan" />
              <Text name="pval" title="equals" placeholder="pro" />
            </div>
            <label style={{ display: "grid", gap: 4, fontSize: 13 }}>
              <span className="muted">Or upload a CSV with a distinct_id or email column (replaces the conditions)</span>
              <input type="file" name="file" accept=".csv,text/csv" />
            </label>
            <div>
              <button className="primary" type="submit">
                Create segment
              </button>
            </div>
          </form>
          {searchParams.made ? (
            <p data-copy-source="product-analytics segments (live)">
              Created {searchParams.made}.{searchParams.unmatched && searchParams.unmatched !== "0" ? ` ${searchParams.unmatched} rows of the file matched nobody.` : ""}
            </p>
          ) : null}
          {problem ? <p className="error">{problem}</p> : null}
        </section>
      ) : null}
      {ctx.api && segments.length ? (
        <section className="card">
          <ul className="list">
            {segments.map((c) => (
              <li key={c.id}>
                <span>
                  {c.name} <span className="muted">({c.kind === "static" ? "from a file" : "from what people did"})</span>
                </span>
                <span className="muted" data-copy-source="product-analytics segments (live)">
                  {c.people_count.toLocaleString("en-US")} people{c.computed_at ? `, updated ${fmtAt(c.computed_at)}` : ""}
                  {c.error ? `, last update failed: ${c.error}` : ""}
                </span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </InsightsShell>
  );
}
