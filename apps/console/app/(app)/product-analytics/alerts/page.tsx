import { redirect } from "next/navigation";
import { a3Context, appQuery, formObject, isRedirect } from "../replay/_parts/context.ts";
import { A3Shell, NO_APP, NOT_CONNECTED, Notice, Problem } from "../replay/_parts/A3Shell.tsx";
import { alertFromForm, PATHS, problemText, subscriptionFromForm, type AlertRow, type MailLogRow, type SubscriptionRow } from "../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

const input = { display: "grid", gap: 4, fontSize: 13 } as const;
const STATE = { ok: "Quiet", firing: "Firing", error: "Could not check" } as const;
const WEEKDAYS = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const when = (iso: string | null) => (iso ? `${iso.replace("T", " ").slice(0, 16)} UTC` : "never");

async function act(form: FormData) {
  "use server";
  const f = formObject(form);
  const ctx = await a3Context(f.app);
  if (!ctx.api) redirect(PATHS.alerts);
  try {
    if (f.op === "alert") await ctx.api.createAlert(alertFromForm(f));
    else if (f.op === "subscription") await ctx.api.createSubscription(subscriptionFromForm(f));
    else if (f.op === "toggle-alert") await ctx.api.updateAlert(f.id, { enabled: f.enabled !== "true" });
    else if (f.op === "check-alert") await ctx.api.updateAlert(f.id, { check_now: true });
    else if (f.op === "delete-alert") await ctx.api.deleteAlert(f.id);
    else if (f.op === "toggle-subscription") await ctx.api.updateSubscription(f.id, { enabled: f.enabled !== "true" });
    else if (f.op === "send-subscription") await ctx.api.updateSubscription(f.id, { send_now: true });
    else if (f.op === "delete-subscription") await ctx.api.deleteSubscription(f.id);
  } catch (e) {
    if (isRedirect(e)) throw e;
    redirect(`${PATHS.alerts}?${appQuery(ctx.app)}&problem=${encodeURIComponent(problemText(e))}`);
  }
  redirect(`${PATHS.alerts}?${appQuery(ctx.app)}`);
}

// Alerts on saved insights and weekly dashboard emails (QY1.4). An alert
// emails once when it starts firing; every email has an unsubscribe link.
export default async function AlertsPage({ searchParams }: { searchParams: { app?: string; problem?: string } }) {
  const ctx = await a3Context(searchParams.app);
  let alerts: AlertRow[] = [];
  let subs: SubscriptionRow[] = [];
  let mailOn = true;
  let insights: { id: string; name: string }[] = [];
  let dashboards: { id: string; name: string }[] = [];
  const logs = new Map<string, MailLogRow[]>();
  let problem = searchParams.problem ?? null;
  if (ctx.api && ctx.insights) {
    try {
      const [a, s, i, d] = await Promise.all([ctx.api.alerts(), ctx.api.subscriptions(), ctx.insights.saved(), ctx.insights.dashboards()]);
      alerts = a.alerts;
      subs = s.subscriptions;
      mailOn = a.mail_configured;
      insights = i.insights.map((x) => ({ id: x.id, name: x.name }));
      dashboards = d.dashboards.map((x) => ({ id: x.id, name: x.name }));
      for (const al of alerts.slice(0, 20)) logs.set(al.id, (await ctx.api.alertLog(al.id)).log.slice(0, 3));
    } catch (e) {
      problem = problemText(e);
    }
  }
  const hidden = (op: string, id = "", extra: Record<string, string> = {}) => (
    <>
      <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
      <input type="hidden" name="op" value={op} />
      {id ? <input type="hidden" name="id" value={id} /> : null}
      {Object.entries(extra).map(([k, v]) => (
        <input key={k} type="hidden" name={k} value={v} />
      ))}
    </>
  );
  const insightName = (id: string) => insights.find((i) => i.id === id)?.name ?? "a removed insight";
  const dashName = (id: string) => dashboards.find((d) => d.id === id)?.name ?? "a removed dashboard";
  return (
    <A3Shell title="Alerts" names={ctx.names} app={ctx.app?.name ?? null} current={PATHS.alerts}>
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.api && !mailOn ? <Notice text="Email is not connected on this deployment yet: alerts are checked and logged, and no email is sent until it is." /> : null}
      <Problem text={problem ?? undefined} />
      {ctx.api ? (
        <>
          <section className="card">
            <h2>Alerts</h2>
            {alerts.length === 0 ? <p className="muted">No alerts yet. Add one below on a saved insight.</p> : null}
            {alerts.map((a) => (
              <div key={a.id} style={{ borderTop: "1px solid #eef0f4", padding: "10px 0" }}>
                <p>
                  <strong>{a.name}</strong> on {insightName(a.insight_id)}:{" "}
                  {a.kind === "threshold" ? `${a.direction} ${a.threshold}` : `${a.sensitivity} standard deviations ${a.direction === "either" ? "either way" : a.direction}`}, checked every {a.check_every}.
                </p>
                <p className="muted">
                  {a.enabled ? STATE[a.state] : "Paused"}
                  {a.last_value !== null ? `, last value ${a.last_value}` : ""}, last checked {when(a.last_checked_at)}
                  {a.last_error ? `: ${a.last_error}` : ""}. Emails {a.recipients.join(", ")}.
                </p>
                {(logs.get(a.id) ?? []).length ? (
                  <p className="muted" style={{ fontSize: 12 }}>
                    Latest emails: {(logs.get(a.id) ?? []).map((l) => `${l.recipient} ${l.status.replace("_", " ")} ${when(l.created_at)}`).join("; ")}
                  </p>
                ) : null}
                <div className="row">
                  <form action={act}>
                    {hidden("toggle-alert", a.id, { enabled: String(a.enabled) })}
                    <button type="submit">{a.enabled ? "Pause" : "Resume"}</button>
                  </form>
                  <form action={act}>
                    {hidden("check-alert", a.id)}
                    <button type="submit">Check now</button>
                  </form>
                  <form action={act}>
                    {hidden("delete-alert", a.id)}
                    <button type="submit">Delete</button>
                  </form>
                </div>
              </div>
            ))}
            {insights.length ? (
              <form action={act} style={{ display: "grid", gap: 10, maxWidth: 640, marginTop: 12 }}>
                {hidden("alert")}
                <label style={input}>
                  <span className="muted">Name</span>
                  <input name="name" required maxLength={200} placeholder="Signups drop" />
                </label>
                <label style={input}>
                  <span className="muted">Saved insight</span>
                  <select name="insight_id">
                    {insights.map((i) => (
                      <option key={i.id} value={i.id}>
                        {i.name}
                      </option>
                    ))}
                  </select>
                </label>
                <div className="row">
                  <label style={input}>
                    <span className="muted">When</span>
                    <select name="kind" defaultValue="threshold">
                      <option value="threshold">The value crosses a number</option>
                      <option value="anomaly">The value is unusual</option>
                    </select>
                  </label>
                  <label style={input}>
                    <span className="muted">Direction</span>
                    <select name="direction" defaultValue="above">
                      <option value="above">Above</option>
                      <option value="below">Below</option>
                      <option value="either">Either way (unusual only)</option>
                    </select>
                  </label>
                  <label style={input}>
                    <span className="muted">Number</span>
                    <input name="threshold" type="number" step="any" />
                  </label>
                  <label style={input}>
                    <span className="muted">Standard deviations</span>
                    <input name="sensitivity" type="number" min={1} max={10} step={0.5} defaultValue={3} />
                  </label>
                  <label style={input}>
                    <span className="muted">Check every</span>
                    <select name="check_every" defaultValue="hour">
                      <option value="hour">Hour</option>
                      <option value="day">Day</option>
                    </select>
                  </label>
                </div>
                <label style={input}>
                  <span className="muted">Email to (one per line, up to 20)</span>
                  <textarea name="recipients" rows={2} required defaultValue={ctx.email} />
                </label>
                <div>
                  <button className="primary" type="submit">
                    Add alert
                  </button>
                </div>
              </form>
            ) : (
              <p className="muted">Save an insight first; alerts watch saved insights.</p>
            )}
          </section>
          <section className="card">
            <h2>Weekly dashboard emails</h2>
            {subs.length === 0 ? <p className="muted">No weekly emails yet.</p> : null}
            {subs.map((s) => (
              <div key={s.id} style={{ borderTop: "1px solid #eef0f4", padding: "10px 0" }}>
                <p>
                  <strong>{dashName(s.dashboard_id)}</strong> every {WEEKDAYS[s.weekday]} at {String(s.hour_utc).padStart(2, "0")}:00 UTC to {s.recipients.join(", ")}.
                </p>
                <p className="muted">
                  {s.enabled ? `Next ${when(s.next_send_at)}` : "Paused"}, last sent {when(s.last_sent_at)}
                  {s.last_error ? `: ${s.last_error}` : ""}.
                </p>
                <div className="row">
                  <form action={act}>
                    {hidden("toggle-subscription", s.id, { enabled: String(s.enabled) })}
                    <button type="submit">{s.enabled ? "Pause" : "Resume"}</button>
                  </form>
                  <form action={act}>
                    {hidden("send-subscription", s.id)}
                    <button type="submit">Send now</button>
                  </form>
                  <form action={act}>
                    {hidden("delete-subscription", s.id)}
                    <button type="submit">Delete</button>
                  </form>
                </div>
              </div>
            ))}
            {dashboards.length ? (
              <form action={act} style={{ display: "grid", gap: 10, maxWidth: 640, marginTop: 12 }}>
                {hidden("subscription")}
                <div className="row">
                  <label style={input}>
                    <span className="muted">Dashboard</span>
                    <select name="dashboard_id">
                      {dashboards.map((d) => (
                        <option key={d.id} value={d.id}>
                          {d.name}
                        </option>
                      ))}
                    </select>
                  </label>
                  <label style={input}>
                    <span className="muted">Day</span>
                    <select name="weekday" defaultValue="1">
                      {WEEKDAYS.map((w, i) => (
                        <option key={w} value={i}>
                          {w}
                        </option>
                      ))}
                    </select>
                  </label>
                  <label style={input}>
                    <span className="muted">Hour (UTC)</span>
                    <input name="hour_utc" type="number" min={0} max={23} defaultValue={8} />
                  </label>
                </div>
                <label style={input}>
                  <span className="muted">Email to (one per line, up to 20)</span>
                  <textarea name="recipients" rows={2} required defaultValue={ctx.email} />
                </label>
                <div>
                  <button className="primary" type="submit">
                    Add weekly email
                  </button>
                </div>
              </form>
            ) : (
              <p className="muted">Make a dashboard first; weekly emails send a dashboard.</p>
            )}
          </section>
        </>
      ) : null}
    </A3Shell>
  );
}
