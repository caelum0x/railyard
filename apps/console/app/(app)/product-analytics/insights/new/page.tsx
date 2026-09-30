import { redirect } from "next/navigation";
import { ResultView } from "../../_components/charts/ResultView.tsx";
import { fmtAt, formToQuery, KINDS, problemText, RANGES, type Answer, type Form, type Meta, type Query } from "../../_lib/insights-client.ts";
import { appQuery, insightsContext } from "../_parts/context.ts";
import { Choice, KindFields, Text } from "../_parts/fields.tsx";
import { InsightsShell, NO_APP, NOT_CONNECTED, Notice } from "../_parts/Nav.tsx";

export const dynamic = "force-dynamic";

async function saveInsight(form: FormData) {
  "use server";
  const ctx = await insightsContext(String(form.get("app") ?? ""));
  if (!ctx.api || !ctx.app) redirect("/product-analytics/insights");
  const query = JSON.parse(String(form.get("q") ?? "{}")) as Query;
  const name = String(form.get("name") ?? "").trim() || "Untitled insight";
  const { insight } = await ctx.api.save(name, query);
  redirect(`/product-analytics/insights/${insight.id}?${appQuery(ctx.app)}`);
}

// The builder: pick a type, fill the fields, show the answer. The one
// action is Show; saving is offered under the answer.
export default async function NewInsightPage({ searchParams }: { searchParams: Form }) {
  const ctx = await insightsContext(searchParams.app);
  const f = searchParams;
  const kind = f.kind ?? "trends";
  const shell = (children: React.ReactNode) => (
    <InsightsShell title="New insight" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/insights">
      {children}
    </InsightsShell>
  );
  if (!ctx.app) return shell(<Notice text={NO_APP} />);
  if (!ctx.api) return shell(<Notice text={NOT_CONNECTED} />);
  let meta: Meta | null = null;
  let answer: Answer | null = null;
  let problem: string | null = null;
  const query = formToQuery(f);
  try {
    meta = await ctx.api.meta();
    if (f.show === "1") answer = await ctx.api.query(query);
  } catch (e) {
    problem = problemText(e);
  }
  const q = appQuery(ctx.app);
  const peopleBase = `/product-analytics/insights/people?${q}&q=${encodeURIComponent(JSON.stringify(query))}`;
  return shell(
    <>
      <section className="card">
        <nav className="row" aria-label="Insight type" style={{ marginBottom: 14, fontSize: 14 }}>
          {KINDS.map((k) => (
            <a key={k.kind} href={`/product-analytics/insights/new?${q}&kind=${k.kind}`} aria-current={k.kind === kind ? "page" : undefined} className={k.kind === kind ? undefined : "muted"}>
              {k.label}
            </a>
          ))}
        </nav>
        {meta ? (
          <form method="get" action="/product-analytics/insights/new">
            <input type="hidden" name="app" value={ctx.app.name} />
            <input type="hidden" name="kind" value={kind} />
            <input type="hidden" name="show" value="1" />
            <KindFields f={f} meta={meta} />
            <div className="row" style={{ alignItems: "end", marginBottom: 12 }}>
              <Choice name="range" value={f.range ?? "-30d"} title="Time range" options={RANGES} />
              {kind === "trends" || kind === "stickiness" || kind === "lifecycle" ? (
                <Choice name="interval" value={f.interval ?? "day"} title="Per" options={["hour", "day", "week", "month"].map((i) => ({ value: i, label: i }))} />
              ) : null}
              <Choice name="segment" value={f.segment} title="People" options={[{ value: "", label: "Everyone" }, ...meta.segments.map((c) => ({ value: c.id, label: c.name }))]} />
              <Text name="pkey" value={f.pkey} title="Person property" placeholder="plan" />
              <Text name="pval" value={f.pval} title="equals" placeholder="pro" />
            </div>
            <button className="primary" type="submit">
              Show
            </button>
          </form>
        ) : null}
        {problem ? <p className="error">{problem}</p> : null}
      </section>
      {answer ? (
        <section className="card">
          <ResultView result={answer.result} dropOffBase={peopleBase} />
          <p className="muted" style={{ fontSize: 12 }} data-copy-source="product-analytics insights query (live)">
            Computed {fmtAt(answer.computed_at)}.
          </p>
          <form action={saveInsight} className="row" style={{ marginTop: 12 }}>
            <input type="hidden" name="app" value={ctx.app.name} />
            <input type="hidden" name="q" value={JSON.stringify(query)} />
            <input type="text" name="name" placeholder="Name this insight" aria-label="Insight name" />
            <button className="ghost" type="submit">
              Save insight
            </button>
          </form>
        </section>
      ) : null}
    </>,
  );
}
