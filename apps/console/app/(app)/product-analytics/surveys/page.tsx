import { a3Context, appQuery } from "../replay/_parts/context.ts";
import { A3Shell, fmt, NO_APP, NOT_CONNECTED, Notice, Problem } from "../replay/_parts/A3Shell.tsx";
import { problemText, type Survey } from "../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

const STATUS: Record<Survey["status"], string> = { draft: "Draft", running: "Showing", stopped: "Stopped" };

// Surveys of one app. One action: create a survey.
export default async function SurveysPage({ searchParams }: { searchParams: { app?: string } }) {
  const ctx = await a3Context(searchParams.app);
  const q = appQuery(ctx.app);
  let surveys: Survey[] = [];
  let max = 0;
  let problem: string | null = null;
  if (ctx.api) {
    try {
      const r = await ctx.api.surveys();
      surveys = r.surveys;
      max = r.max_running;
    } catch (e) {
      problem = problemText(e);
    }
  }
  const target = (s: Survey) => (s.url ? `pages matching ${s.url}` : s.flag_key ? `people with the ${s.flag_key} flag on` : s.cohort_id ? "a segment" : "everyone");
  return (
    <A3Shell title="Surveys" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/surveys">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.api ? (
        <section className="card">
          <p>
            <a className="primary" href={`/product-analytics/surveys/new?${q}`}>
              Create a survey
            </a>
          </p>
          <p className="muted" data-copy-source="product-analytics src/surveys/store.ts MAX_RUNNING">
            Surveys show in the web and mobile apps that use this app&apos;s analytics setup; answers arrive as events. Up to {max} surveys show at once.
          </p>
          <Problem text={problem} />
          {surveys.length ? (
            <table>
              <thead>
                <tr>
                  <th>Survey</th>
                  <th>Status</th>
                  <th>Shown to</th>
                  <th>Created</th>
                </tr>
              </thead>
              <tbody data-copy-source="product-analytics /surveys (live)">
                {surveys.map((s) => (
                  <tr key={s.id}>
                    <td>
                      <a href={`/product-analytics/surveys/${s.id}?${q}`}>{s.name}</a>
                    </td>
                    <td>{STATUS[s.status]}</td>
                    <td>{target(s)}</td>
                    <td>{fmt(s.created_at)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : (
            <p className="muted">No surveys yet.</p>
          )}
        </section>
      ) : null}
    </A3Shell>
  );
}
