import { redirect } from "next/navigation";
import { a3Context, appQuery, field, isRedirect } from "../../replay/_parts/context.ts";
import { A3Shell, fmt, NO_APP, NOT_CONNECTED, Notice, Problem, ShareBar } from "../../replay/_parts/A3Shell.tsx";
import { answerKeys, problemText, type Survey, type SurveyResults } from "../../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

async function act(form: FormData) {
  "use server";
  const ctx = await a3Context(field(form, "app", 200));
  const id = field(form, "id", 64);
  const what = field(form, "what", 20);
  if (!ctx.api) redirect("/product-analytics/surveys");
  const back = `/product-analytics/surveys/${id}?${appQuery(ctx.app)}`;
  try {
    if (what === "start") await ctx.api.setSurveyStatus(id, "running");
    else if (what === "stop") await ctx.api.setSurveyStatus(id, "stopped");
    else if (what === "delete") {
      await ctx.api.deleteSurvey(id);
      redirect(`/product-analytics/surveys?${appQuery(ctx.app)}`);
    }
  } catch (e) {
    if (isRedirect(e)) throw e;
    redirect(`${back}&problem=${encodeURIComponent(problemText(e))}`);
  }
  redirect(back);
}

// One survey: its answers, counted from the answer events. One action: start
// or stop it.
export default async function SurveyPage({ params, searchParams }: { params: { id: string }; searchParams: { app?: string; problem?: string } }) {
  const ctx = await a3Context(searchParams.app);
  let survey: Survey | null = null;
  let results: SurveyResults | null = null;
  let problem: string | null = searchParams.problem ?? null;
  if (ctx.api) {
    try {
      [{ survey }, { results }] = await Promise.all([ctx.api.survey(params.id), ctx.api.surveyResults(params.id)]);
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
  return (
    <A3Shell title={survey?.name ?? "Survey"} names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/surveys">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      <Problem text={problem} />
      {survey && results ? (
        <>
          <section className="card">
            <div className="row" style={{ alignItems: "center" }}>
              {survey.status !== "stopped" ? (
                <form action={act}>
                  {hidden(survey.status === "draft" ? "start" : "stop")}
                  <button className="primary" type="submit">
                    {survey.status === "draft" ? "Start showing it" : "Stop showing it"}
                  </button>
                </form>
              ) : null}
              {survey.status !== "running" ? (
                <form action={act}>
                  {hidden("delete")}
                  <button type="submit">Delete</button>
                </form>
              ) : null}
            </div>
            <p className="muted" data-copy-source="product-analytics /surveys/{id}/results (live)">
              {survey.status === "running" ? `Showing since ${fmt(survey.start_date)}.` : survey.status === "stopped" ? `Stopped ${fmt(survey.end_date)}.` : "Draft: not shown yet."} Shown to {results.shown.toLocaleString("en-US")} people, {results.dismissed.toLocaleString("en-US")} closed it,{" "}
              {results.responses.toLocaleString("en-US")} answered.
            </p>
          </section>
          {results.questions.map((q) => (
            <section className="card" key={q.id}>
              <h2>{q.question}</h2>
              <p className="muted">{q.answered.toLocaleString("en-US")} answers</p>
              {q.type === "open" || q.type === "link" ? (
                q.texts.length ? (
                  <ul className="list">
                    {q.texts.map((t, i) => (
                      <li key={i}>{t}</li>
                    ))}
                  </ul>
                ) : null
              ) : (
                <div style={{ display: "grid", gap: 6 }} data-copy-source="product-analytics /surveys/{id}/results (live)">
                  {answerKeys(survey!.questions.find((x) => x.id === q.id), q.counts).map((k) => (
                    <ShareBar key={k} label={k} value={q.counts[k] ?? 0} total={q.answered} />
                  ))}
                </div>
              )}
            </section>
          ))}
        </>
      ) : null}
    </A3Shell>
  );
}
