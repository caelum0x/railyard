import { redirect } from "next/navigation";
import { problemText, type Action } from "../_lib/insights-client.ts";
import { appQuery, insightsContext } from "../insights/_parts/context.ts";
import { Choice, Text } from "../insights/_parts/fields.tsx";
import { InsightsShell, NO_APP, NOT_CONNECTED, Notice } from "../insights/_parts/Nav.tsx";

export const dynamic = "force-dynamic";

async function create(form: FormData) {
  "use server";
  const ctx = await insightsContext(String(form.get("app") ?? ""));
  if (!ctx.api || !ctx.app) redirect("/product-analytics/actions");
  const back = `/product-analytics/actions?${appQuery(ctx.app)}`;
  const what = String(form.get("what") ?? "click");
  const val = (k: string) => String(form.get(k) ?? "").trim() || null;
  const step: Record<string, unknown> =
    what === "click"
      ? { event: "$autocapture", selector: val("selector"), text: val("text"), text_matching: "exact", href: val("href"), href_matching: "contains" }
      : what === "page"
        ? { event: "$pageview", url: val("url"), url_matching: "contains" }
        : { event: val("event") };
  try {
    await ctx.api.createAction(String(form.get("name") ?? ""), [step]);
  } catch (e) {
    redirect(`${back}&problem=${encodeURIComponent(problemText(e))}`);
  }
  redirect(back);
}

// Actions: a name for a click, a page or an event, usable in every
// insight. One action: create one.
export default async function ActionsPage({ searchParams }: { searchParams: { app?: string; problem?: string } }) {
  const ctx = await insightsContext(searchParams.app);
  let actions: Action[] = [];
  let problem: string | null = searchParams.problem ?? null;
  if (ctx.api) {
    try {
      ({ actions } = await ctx.api.actions());
    } catch (e) {
      problem = problemText(e);
    }
  }
  const describe = (s: Record<string, unknown>) =>
    s.event === "$autocapture"
      ? `Click${s.selector ? ` on ${s.selector}` : ""}${s.text ? ` with text "${s.text}"` : ""}${s.href ? ` linking to ${s.href}` : ""}`
      : s.event === "$pageview"
        ? `Page view${s.url ? ` of an address containing ${s.url}` : ""}`
        : `Event ${s.event}`;
  return (
    <InsightsShell title="Actions" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/actions">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.app && ctx.api ? (
        <section className="card">
          <form action={create} style={{ display: "grid", gap: 12 }}>
            <input type="hidden" name="app" value={ctx.app.name} />
            <div className="row" style={{ alignItems: "end" }}>
              <Text name="name" title="Name" placeholder="Clicked buy" />
              <Choice name="what" title="Matches" options={[{ value: "click", label: "A click" }, { value: "page", label: "A page view" }, { value: "event", label: "An event" }]} />
            </div>
            <div className="row" style={{ alignItems: "end" }}>
              <Text name="selector" title="Click: CSS selector" placeholder="form button.primary" />
              <Text name="text" title="Click: button text" placeholder="Buy now" />
              <Text name="href" title="Click: link contains" placeholder="/pricing" />
            </div>
            <div className="row" style={{ alignItems: "end" }}>
              <Text name="url" title="Page view: address contains" placeholder="/signup" />
              <Text name="event" title="Event name" placeholder="signup" />
            </div>
            <div>
              <button className="primary" type="submit">
                Create action
              </button>
            </div>
          </form>
          {problem ? <p className="error">{problem}</p> : null}
        </section>
      ) : null}
      {actions.length ? (
        <section className="card">
          <ul className="list">
            {actions.map((a) => (
              <li key={a.id}>
                <span>{a.name}</span>
                <span className="muted">{a.steps.map(describe).join(" or ")}</span>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </InsightsShell>
  );
}
