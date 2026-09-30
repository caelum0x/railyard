import { cookies } from "next/headers";
import { redirect } from "next/navigation";
import { a3Context, appQuery, field, isRedirect } from "../../replay/_parts/context.ts";
import { A3Shell, fmt, NO_APP, NOT_CONNECTED, Notice, Problem } from "../../replay/_parts/A3Shell.tsx";
import { problemText, STATUS_TEXT, type Destination, type ExportRun } from "../../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

const SECRET_COOKIE = "pa_export_secret";

async function act(form: FormData) {
  "use server";
  const ctx = await a3Context(field(form, "app", 200));
  const id = field(form, "id", 64);
  const what = field(form, "what", 20);
  if (!ctx.api) redirect("/product-analytics/exports");
  const back = `/product-analytics/exports/${id}?${appQuery(ctx.app)}`;
  let note = "";
  try {
    if (what === "pause") await ctx.api.updateDestination(id, { enabled: false });
    else if (what === "resume") await ctx.api.updateDestination(id, { enabled: true, retry_now: true });
    else if (what === "retry") await ctx.api.updateDestination(id, { retry_now: true });
    else if (what === "hide") cookies().delete({ name: SECRET_COOKIE, path: "/product-analytics/exports" });
    else if (what === "test") {
      const r = await ctx.api.testDestination(id);
      note = r.ok ? `The webhook answered ${r.status}.` : r.error ?? "The webhook did not answer.";
    } else if (what === "delete") {
      await ctx.api.deleteDestination(id);
      redirect(`/product-analytics/exports?${appQuery(ctx.app)}`);
    }
  } catch (e) {
    if (isRedirect(e)) throw e;
    redirect(`${back}&problem=${encodeURIComponent(problemText(e))}`);
  }
  redirect(note ? `${back}&note=${encodeURIComponent(note)}` : back);
}

function readSecret(id: string): string | null {
  try {
    const v = JSON.parse(cookies().get(SECRET_COOKIE)?.value ?? "null") as { id?: string; secret?: string } | null;
    return v && v.id === id && typeof v.secret === "string" ? v.secret : null;
  } catch {
    return null;
  }
}

// One destination: its state, its recent deliveries, and pause, resume, retry,
// test and delete. One action: pause or resume.
export default async function DestinationPage({ params, searchParams }: { params: { id: string }; searchParams: { app?: string; problem?: string; note?: string } }) {
  const ctx = await a3Context(searchParams.app);
  let d: Destination | null = null;
  let runs: ExportRun[] = [];
  let problem: string | null = searchParams.problem ?? null;
  if (ctx.api) {
    try {
      [{ destination: d }, { runs }] = await Promise.all([ctx.api.destination(params.id), ctx.api.runs(params.id)]);
    } catch (e) {
      problem = problem ?? problemText(e);
    }
  }
  const secret = d ? readSecret(d.id) : null;
  const hidden = (what: string) => (
    <>
      <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
      <input type="hidden" name="id" value={params.id} />
      <input type="hidden" name="what" value={what} />
    </>
  );
  return (
    <A3Shell title={d?.name ?? "Destination"} names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/exports">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      <Problem text={problem} />
      {searchParams.note ? (
        <section className="card">
          <p>{searchParams.note}</p>
        </section>
      ) : null}
      {secret ? (
        <section className="card">
          <h2>Signing secret</h2>
          <p>Copy it now; it is not shown again. Every batch carries webhook-id, webhook-timestamp and webhook-signature headers (Standard Webhooks) signed with it.</p>
          <p>
            <code>{secret}</code>
          </p>
          <form action={act}>
            {hidden("hide")}
            <button type="submit">I saved it</button>
          </form>
        </section>
      ) : null}
      {d ? (
        <>
          <section className="card">
            <p className="muted" data-copy-source="product-analytics /exports/{id} (live)">
              {d.kind === "webhook" ? `Webhook ${d.config.url}` : `Bucket ${d.config.bucket} at ${d.config.endpoint}${d.config.prefix ? `, folder ${d.config.prefix}` : ""}`}.{" "}
              {d.events ? `Only ${d.events.join(", ")}.` : "Every event."} {d.enabled ? STATUS_TEXT[d.status] : "Paused"}. {d.delivered.toLocaleString("en-US")} events sent
              {d.last_success_at ? `, last at ${fmt(d.last_success_at)}` : ""}.
              {d.last_error ? ` Last error: ${d.last_error}` : ""}
              {d.next_attempt_at && d.status !== "ok" ? ` Next try ${fmt(d.next_attempt_at)}.` : ""}
            </p>
            <div className="row">
              <form action={act}>
                {hidden(d.enabled ? "pause" : "resume")}
                <button className="primary" type="submit">
                  {d.enabled ? "Pause" : "Resume"}
                </button>
              </form>
              {d.enabled && d.status !== "ok" ? (
                <form action={act}>
                  {hidden("retry")}
                  <button type="submit">Try again now</button>
                </form>
              ) : null}
              {d.kind === "webhook" ? (
                <form action={act}>
                  {hidden("test")}
                  <button type="submit">Send a test</button>
                </form>
              ) : null}
              <form action={act}>
                {hidden("delete")}
                <button type="submit">Delete</button>
              </form>
            </div>
          </section>
          <section className="card">
            <h2>Recent deliveries</h2>
            {runs.length ? (
              <table>
                <thead>
                  <tr>
                    <th>When</th>
                    <th>Result</th>
                    <th>Events</th>
                    <th>Try</th>
                    <th>Detail</th>
                  </tr>
                </thead>
                <tbody data-copy-source="product-analytics /exports/{id}/runs (live)">
                  {runs.map((r, i) => (
                    <tr key={i}>
                      <td>{fmt(r.started_at)}</td>
                      <td>{r.status === "ok" ? "Delivered" : "Failed"}</td>
                      <td>{r.rows}</td>
                      <td>{r.attempt}</td>
                      <td>{r.error ?? (r.http_status ? `HTTP ${r.http_status}` : "")}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            ) : (
              <p className="muted">Nothing sent yet.</p>
            )}
          </section>
        </>
      ) : null}
    </A3Shell>
  );
}
