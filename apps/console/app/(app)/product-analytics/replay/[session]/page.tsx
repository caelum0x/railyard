import { redirect } from "next/navigation";
import { a3Context, appQuery, field, isRedirect } from "../_parts/context.ts";
import { A3Shell, fmt, NO_APP, NOT_CONNECTED, Notice, Problem } from "../_parts/A3Shell.tsx";
import { ARRIVING, liveModules } from "../_parts/live.ts";
import { duration, problemText, type Replay } from "../../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

async function remove(form: FormData) {
  "use server";
  if (!liveModules().replay) redirect("/product-analytics/replay");
  const ctx = await a3Context(field(form, "app", 200));
  const session = field(form, "session", 200);
  if (!ctx.api) redirect("/product-analytics/replay");
  try {
    if (form.get("confirm") !== "yes") redirect(`/product-analytics/replay/${encodeURIComponent(session)}?${appQuery(ctx.app)}&problem=${encodeURIComponent("Tick the box to confirm.")}`);
    await ctx.api.deleteReplay(session);
  } catch (e) {
    if (isRedirect(e)) throw e;
    redirect(`/product-analytics/replay/${encodeURIComponent(session)}?${appQuery(ctx.app)}&problem=${encodeURIComponent(problemText(e))}`);
  }
  redirect(`/product-analytics/replay?${appQuery(ctx.app)}`);
}

// One recording, played in the service's player page through a signed link
// that works for 10 minutes. One action: watch (deleting is secondary).
export default async function ReplayPage({ params, searchParams }: { params: { session: string }; searchParams: { app?: string; t?: string; problem?: string } }) {
  const ctx = await a3Context(searchParams.app);
  if (!liveModules().replay) {
    return (
      <A3Shell title="Arriving" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/replay">
        <Notice text={ARRIVING} />
      </A3Shell>
    );
  }
  const session = decodeURIComponent(params.session);
  let replay: Replay | null = null;
  let link: { url: string; expires_at: string } | null = null;
  let problem: string | null = searchParams.problem ?? null;
  if (ctx.api) {
    try {
      replay = (await ctx.api.replay(session)).replay;
      link = await ctx.api.replayLink(session, Math.max(0, Number(searchParams.t ?? "0") || 0));
    } catch (e) {
      problem = problemText(e);
    }
  }
  return (
    <A3Shell title="Recording" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/replay">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      <Problem text={problem} />
      {replay && link ? (
        <>
          <section className="card">
            <p className="muted" data-copy-source="product-analytics /replays/{session} (live)">
              {fmt(replay.started_at)}, {duration(replay.duration_ms)}, {replay.click_count} clicks, {replay.keypress_count} typed inputs (masked).{" "}
              <a href={`/product-analytics/persons/${encodeURIComponent(replay.distinct_id)}?${appQuery(ctx.app)}`}>{replay.distinct_id}</a>. Kept until {fmt(replay.expires_at)}.
            </p>
            <iframe title="Recording player" src={link.url} style={{ width: "100%", height: "78vh", border: "1px solid #d9dde5", borderRadius: 8 }} sandbox="allow-scripts allow-same-origin" referrerPolicy="no-referrer" />
            <p className="muted" data-copy-source="product-analytics signed link (10 minutes)">
              Share this player link with a teammate; it works without signing in until {fmt(link.expires_at)}: <a href={link.url}>open the player</a>.
            </p>
          </section>
          <section className="card">
            <form action={remove} className="row" style={{ alignItems: "center" }}>
              <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
              <input type="hidden" name="session" value={session} />
              <label style={{ fontSize: 14 }}>
                <input type="checkbox" name="confirm" value="yes" /> Delete this recording for good
              </label>
              <button type="submit">Delete</button>
            </form>
          </section>
        </>
      ) : null}
    </A3Shell>
  );
}
