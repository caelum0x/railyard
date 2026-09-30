import { redirect } from "next/navigation";
import { a3Context, appQuery, field, isRedirect } from "../../replay/_parts/context.ts";
import { A3Shell, fmt, NO_APP, NOT_CONNECTED, Notice, Problem } from "../../replay/_parts/A3Shell.tsx";
import { ARRIVING, liveModules } from "../../replay/_parts/live.ts";
import { percent, problemText, type Experiment, type ExperimentResults, type FlagState } from "../../_lib/a3-client.ts";
import { flagStateText } from "../_parts/text.ts";
import { changeExperiment, connectedSlot, ExperimentChangeError } from "../_slot/flags-slot.ts";

export const dynamic = "force-dynamic";

async function act(form: FormData) {
  "use server";
  if (!liveModules().flags) redirect("/product-analytics/experiments");
  const ctx = await a3Context(field(form, "app", 200));
  const id = field(form, "id", 64);
  const what = field(form, "what", 20);
  if (!ctx.api) redirect("/product-analytics/experiments");
  const back = `/product-analytics/experiments/${id}?${appQuery(ctx.app)}`;
  try {
    if (what === "start" || what === "stop") {
      await changeExperiment(ctx.api, connectedSlot(), ctx.org, id, what, what === "stop" ? field(form, "keep", 40) || "control" : undefined, `experiment:${ctx.email}`, new Date());
    } else if (what === "delete") {
      await ctx.api.deleteExperiment(id);
      redirect(`/product-analytics/experiments?${appQuery(ctx.app)}`);
    }
  } catch (e) {
    if (isRedirect(e)) throw e;
    redirect(`${back}&problem=${encodeURIComponent(e instanceof ExperimentChangeError ? e.message : problemText(e))}`);
  }
  redirect(back);
}

const num = (v: number | null, digits = 3) => (v === null ? "" : v.toFixed(digits));

// One experiment: whether its flag carries it, who was exposed to which
// variant, and how each variant compares with control. One action: start or stop.
export default async function ExperimentPage({ params, searchParams }: { params: { id: string }; searchParams: { app?: string; problem?: string } }) {
  const ctx = await a3Context(searchParams.app);
  if (!liveModules().flags) {
    return (
      <A3Shell title="Arriving" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/experiments">
        <Notice text={ARRIVING} />
      </A3Shell>
    );
  }
  let exp: Experiment | null = null;
  let state: FlagState = "not_connected";
  let results: ExperimentResults | null = null;
  let problem: string | null = searchParams.problem ?? null;
  if (ctx.api) {
    try {
      const d = await ctx.api.experiment(params.id);
      exp = d.experiment;
      state = d.flag_state;
      results = (await ctx.api.experimentResults(params.id)).results;
    } catch (e) {
      problem = problem ?? problemText(e);
    }
  }
  const hidden = (what: string) => (
    <>
      <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
      <input type="hidden" name="id" value={params.id} />
      <input type="hidden" name="what" value={what} />
    </>
  );
  const metric = exp ? (exp.metric.kind === "conversion" ? `share of people who did ${exp.metric.event}` : exp.metric.kind === "count" ? `times per person they did ${exp.metric.event}` : `sum of ${exp.metric.property} of ${exp.metric.event} per person`) : "";
  return (
    <A3Shell title={exp?.name ?? "Experiment"} names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/experiments">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      <Problem text={problem} />
      {exp ? (
        <section className="card">
          <p className="muted" data-copy-source="product-analytics /experiments/{id} (live)">
            Flag {exp.flag_key} in {exp.flags_env}: {exp.variants.map((v) => `${v.key} ${v.weight}%`).join(", ")}, {exp.rollout}% of people. Measures the {metric}. {flagStateText(state)}
          </p>
          <div className="row" style={{ alignItems: "end" }}>
            {exp.status !== "stopped" ? (
              <form action={act} className="row" style={{ alignItems: "end" }}>
                {hidden(exp.status === "draft" ? "start" : "stop")}
                {exp.status === "running" ? (
                  <label style={{ display: "grid", gap: 4, fontSize: 13 }}>
                    <span className="muted">From now on everyone gets</span>
                    <select name="keep" defaultValue="control">
                      {exp.variants.map((v) => (
                        <option key={v.key} value={v.key}>
                          {v.key}
                        </option>
                      ))}
                    </select>
                  </label>
                ) : null}
                <button className="primary" type="submit">
                  {exp.status === "draft" ? "Start the experiment" : "Stop the experiment"}
                </button>
              </form>
            ) : null}
            {exp.status !== "running" ? (
              <form action={act}>
                {hidden("delete")}
                <button type="submit">Delete</button>
              </form>
            ) : null}
          </div>
          {exp.started_at ? <p className="muted">Started {fmt(exp.started_at)}{exp.stopped_at ? `, stopped ${fmt(exp.stopped_at)}` : ""}.</p> : null}
        </section>
      ) : null}
      {exp && results ? (
        <section className="card">
          <h2>Results</h2>
          {results.srm.warning ? <p className="error">{results.srm.warning}</p> : null}
          <table>
            <thead>
              <tr>
                <th>Variant</th>
                <th>Planned</th>
                <th>People exposed</th>
                <th>{exp.metric.kind === "conversion" ? "Converted" : "Mean per person"}</th>
                <th>{exp.method === "bayesian" ? "Chance to beat control" : "p-value"}</th>
                <th>Change against control (95% interval)</th>
              </tr>
            </thead>
            <tbody data-copy-source="product-analytics /experiments/{id}/results (live, ported statistics)">
              {results.variants.map((v) => {
                const c = results!.comparisons.find((x) => x.variant === v.variant);
                return (
                  <tr key={v.variant}>
                    <td>{v.variant}</td>
                    <td>{v.planned_weight}%</td>
                    <td>{v.exposed.toLocaleString("en-US")}</td>
                    <td>{v.mean === null ? "" : exp!.metric.kind === "conversion" ? `${v.converted.toLocaleString("en-US")} (${percent(v.mean)})` : num(v.mean, 2)}</td>
                    <td>{!c ? "" : c.error ? c.error : exp!.method === "bayesian" ? percent(c.chance_to_win ?? 0) : num(c.p_value, 4)}</td>
                    <td>{c && c.effect !== null && c.ci ? `${percent(c.effect)} (${percent(c.ci[0])} to ${percent(c.ci[1])})` : ""}</td>
                  </tr>
                );
              })}
            </tbody>
          </table>
          <p className="muted">
            Exposure is the first time the app read the flag for a person. {results.multiple_variants ? `${results.multiple_variants} people saw more than one variant and are left out.` : ""}
          </p>
        </section>
      ) : null}
    </A3Shell>
  );
}
