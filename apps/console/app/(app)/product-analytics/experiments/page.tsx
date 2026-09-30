import { a3Context, appQuery } from "../replay/_parts/context.ts";
import { A3Shell, fmt, NO_APP, NOT_CONNECTED, Notice, Problem } from "../replay/_parts/A3Shell.tsx";
import { ARRIVING, liveModules } from "../replay/_parts/live.ts";
import { PATHS, problemText, type Experiment } from "../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

const STATUS: Record<Experiment["status"], string> = { draft: "Draft", running: "Running", stopped: "Stopped" };

// Experiments of one app, each on a flag of the flags slot. One action: create one.
export default async function ExperimentsPage({ searchParams }: { searchParams: { app?: string } }) {
  const ctx = await a3Context(searchParams.app);
  if (!liveModules().flags) {
    return (
      <A3Shell title="Arriving" names={ctx.names} app={ctx.app?.name ?? null} current={PATHS.exp}>
        <Notice text={ARRIVING} />
      </A3Shell>
    );
  }
  const q = appQuery(ctx.app);
  let list: Experiment[] = [];
  let problem: string | null = null;
  if (ctx.api) {
    try {
      list = (await ctx.api.experiments()).experiments;
    } catch (e) {
      problem = problemText(e);
    }
  }
  return (
    <A3Shell title="Variant tests" names={ctx.names} app={ctx.app?.name ?? null} current={PATHS.exp}>
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.api ? (
        <section className="card">
          <p>
            <a className="primary" href={`${PATHS.exp}/new?${q}`}>
              Create an experiment
            </a>
          </p>
          <p className="muted">An experiment splits people between variants of one flag and compares what they do next.</p>
          <Problem text={problem} />
          {list.length ? (
            <table>
              <thead>
                <tr>
                  <th>Experiment</th>
                  <th>Flag</th>
                  <th>Status</th>
                  <th>Started</th>
                </tr>
              </thead>
              <tbody data-copy-source="product-analytics experiment API (live)">
                {list.map((x) => (
                  <tr key={x.id}>
                    <td>
                      <a href={`${PATHS.exp}/${x.id}?${q}`}>{x.name}</a>
                    </td>
                    <td>{x.flag_key}</td>
                    <td>{STATUS[x.status]}</td>
                    <td>{fmt(x.started_at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : (
            <p className="muted">No variant tests yet.</p>
          )}
        </section>
      ) : null}
    </A3Shell>
  );
}
