import { redirect } from "next/navigation";
import { a3Context, appQuery, formObject, isRedirect } from "../../replay/_parts/context.ts";
import { A3Shell, NO_APP, NOT_CONNECTED, Notice, Problem } from "../../replay/_parts/A3Shell.tsx";
import { ARRIVING, liveModules } from "../../replay/_parts/live.ts";
import { experimentFromForm, PATHS, problemText } from "../../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

async function create(form: FormData) {
  "use server";
  if (!liveModules().flags) redirect(PATHS.exp);
  const f = formObject(form);
  const ctx = await a3Context(f.app);
  if (!ctx.api) redirect(PATHS.exp);
  let id = "";
  try {
    id = (await ctx.api.createExperiment(experimentFromForm(f))).experiment.id;
  } catch (e) {
    if (isRedirect(e)) throw e;
    redirect(`${PATHS.exp}/new?${appQuery(ctx.app)}&problem=${encodeURIComponent(problemText(e))}`);
  }
  redirect(`${PATHS.exp}/${id}?${appQuery(ctx.app)}`);
}

const input = { display: "grid", gap: 4, fontSize: 13 } as const;

// A new experiment: the flag, the variants and their split, and the metric.
// One action: create (it starts from its own page).
export default async function NewExperimentPage({ searchParams }: { searchParams: { app?: string; problem?: string } }) {
  const ctx = await a3Context(searchParams.app);
  if (!liveModules().flags) {
    return (
      <A3Shell title="Arriving" names={ctx.names} app={ctx.app?.name ?? null} current={PATHS.exp}>
        <Notice text={ARRIVING} />
      </A3Shell>
    );
  }
  return (
    <A3Shell title="New experiment" names={ctx.names} app={ctx.app?.name ?? null} current={PATHS.exp}>
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.api ? (
        <section className="card">
          <form action={create} style={{ display: "grid", gap: 12, maxWidth: 640 }}>
            <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
            <label style={input}>
              <span className="muted">Name</span>
              <input name="name" required maxLength={200} placeholder="Shorter checkout" />
            </label>
            <div className="row" style={{ alignItems: "end" }}>
              <label style={input}>
                <span className="muted">Flag key (your app reads this flag)</span>
                <input name="flag_key" required pattern="[a-z][a-z0-9_]{0,63}" placeholder="new_checkout" />
              </label>
              <label style={input}>
                <span className="muted">Flags environment</span>
                <input name="flags_env" defaultValue="production" />
              </label>
            </div>
            <label style={input}>
              <span className="muted">Variants and their share, one per line (one must be control; shares add up to 100)</span>
              <textarea name="variants" rows={3} defaultValue={"control 50\ntest 50"} />
            </label>
            <label style={input}>
              <span className="muted">Share of people in the experiment (percent)</span>
              <input name="rollout" type="number" min={1} max={100} defaultValue={100} style={{ width: 90 }} />
            </label>
            <div className="row" style={{ alignItems: "end" }}>
              <label style={input}>
                <span className="muted">Measure</span>
                <select name="metric_kind" defaultValue="conversion">
                  <option value="conversion">Share of people who did</option>
                  <option value="count">Times per person they did</option>
                  <option value="sum">Sum per person of a property of</option>
                </select>
              </label>
              <label style={input}>
                <span className="muted">the event</span>
                <input name="metric_event" required placeholder="purchase" />
              </label>
              <label style={input}>
                <span className="muted">property (for a sum)</span>
                <input name="metric_property" placeholder="amount" />
              </label>
            </div>
            <label style={input}>
              <span className="muted">Statistics</span>
              <select name="method" defaultValue="bayesian">
                <option value="bayesian">Chance to win (Bayesian)</option>
                <option value="frequentist">p-value and confidence interval</option>
              </select>
            </label>
            <div>
              <button className="primary" type="submit">
                Create experiment
              </button>
            </div>
          </form>
          <Problem text={searchParams.problem} />
        </section>
      ) : null}
    </A3Shell>
  );
}
