import { redirect } from "next/navigation";
import { a3Context, appQuery, formObject, isRedirect } from "../../replay/_parts/context.ts";
import { A3Shell, NO_APP, NOT_CONNECTED, Notice, Problem } from "../../replay/_parts/A3Shell.tsx";
import { problemText, surveyFromForm } from "../../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

async function create(form: FormData) {
  "use server";
  const f = formObject(form);
  const ctx = await a3Context(f.app);
  if (!ctx.api) redirect("/product-analytics/surveys");
  let id = "";
  try {
    id = (await ctx.api.createSurvey(surveyFromForm(f))).survey.id;
  } catch (e) {
    if (isRedirect(e)) throw e;
    redirect(`/product-analytics/surveys/new?${appQuery(ctx.app)}&problem=${encodeURIComponent(problemText(e))}`);
  }
  redirect(`/product-analytics/surveys/${id}?${appQuery(ctx.app)}`);
}

const input = { display: "grid", gap: 4, fontSize: 13 } as const;

// A new survey: one question, where it shows, and who sees it. One action: create.
export default async function NewSurveyPage({ searchParams }: { searchParams: { app?: string; problem?: string } }) {
  const ctx = await a3Context(searchParams.app);
  let segments: { id: string; name: string }[] = [];
  let problem: string | null = searchParams.problem ?? null;
  if (ctx.insights) {
    try {
      segments = (await ctx.insights.cohorts()).segments.map((c) => ({ id: c.id, name: c.name }));
    } catch (e) {
      problem = problem ?? problemText(e);
    }
  }
  return (
    <A3Shell title="New survey" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/surveys">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.api ? (
        <section className="card">
          <form action={create} style={{ display: "grid", gap: 12, maxWidth: 640 }}>
            <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
            <label style={input}>
              <span className="muted">Name (only you see it)</span>
              <input name="name" required maxLength={200} placeholder="Checkout feedback" />
            </label>
            <label style={input}>
              <span className="muted">Question</span>
              <input name="question" required maxLength={500} placeholder="What almost stopped you from buying?" />
            </label>
            <div className="row" style={{ alignItems: "end" }}>
              <label style={input}>
                <span className="muted">Answer as</span>
                <select name="qtype" defaultValue="single_choice">
                  <option value="single_choice">One choice</option>
                  <option value="multiple_choice">Several choices</option>
                  <option value="rating">A rating</option>
                  <option value="open">Free text</option>
                  <option value="link">A link to open</option>
                </select>
              </label>
              <label style={input}>
                <span className="muted">Rating scale</span>
                <select name="scale" defaultValue="5">
                  {[3, 5, 7, 10].map((n) => (
                    <option key={n} value={n}>
                      {n === 10 ? "0 to 10" : `1 to ${n}`}
                    </option>
                  ))}
                </select>
              </label>
              <label style={input}>
                <span className="muted">Button text</span>
                <input name="button" maxLength={50} placeholder="Send" />
              </label>
            </div>
            <label style={input}>
              <span className="muted">Choices, one per line</span>
              <textarea name="choices" rows={4} placeholder={"Price\nShipping\nNothing"} />
            </label>
            <label style={input}>
              <span className="muted">Link (for a link question)</span>
              <input name="link" placeholder="https://shop.example/help" />
            </label>
            <fieldset style={{ display: "grid", gap: 8, border: "1px solid #d9dde5", borderRadius: 8, padding: 12 }}>
              <legend className="muted" style={{ fontSize: 13 }}>
                Who sees it
              </legend>
              <label>
                <input type="radio" name="target" value="everyone" defaultChecked /> Everyone
              </label>
              <label className="row" style={{ alignItems: "center" }}>
                <input type="radio" name="target" value="url" /> On pages whose address
                <select name="url_match" defaultValue="icontains">
                  <option value="icontains">contains</option>
                  <option value="exact">is exactly</option>
                  <option value="regex">matches the pattern</option>
                </select>
                <input name="url" placeholder="/checkout" /> <span className="muted">(web only)</span>
              </label>
              <label className="row" style={{ alignItems: "center" }}>
                <input type="radio" name="target" value="flag" /> People with the flag <input name="flag_key" placeholder="new_checkout" /> on
              </label>
              {segments.length ? (
                <label className="row" style={{ alignItems: "center" }}>
                  <input type="radio" name="target" value="segment" /> People in the segment
                  <select name="segment_id">
                    {segments.map((s) => (
                      <option key={s.id} value={s.id}>
                        {s.name}
                      </option>
                    ))}
                  </select>
                </label>
              ) : null}
            </fieldset>
            <div className="row" style={{ alignItems: "end" }}>
              <label style={input}>
                <span className="muted">Show as</span>
                <select name="display" defaultValue="popover">
                  <option value="popover">A pop-up in the app</option>
                  <option value="api">Nothing (my app shows it)</option>
                </select>
              </label>
              <label style={input}>
                <span className="muted">Stop after this many answers (optional)</span>
                <input name="limit" type="number" min={1} style={{ width: 120 }} />
              </label>
            </div>
            <div>
              <button className="primary" type="submit">
                Create survey
              </button>
            </div>
            <p className="muted">A new survey is a draft; start it from its page.</p>
          </form>
          <Problem text={problem} />
        </section>
      ) : null}
    </A3Shell>
  );
}
