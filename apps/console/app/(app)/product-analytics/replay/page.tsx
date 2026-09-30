import { a3Context, appQuery } from "./_parts/context.ts";
import { A3Shell, fmt, NO_APP, NOT_CONNECTED, Notice, Problem } from "./_parts/A3Shell.tsx";
import { ARRIVING, liveModules } from "./_parts/live.ts";
import { duration, problemText, type Replay } from "../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

interface Params {
  app?: string;
  person?: string;
  event?: string;
  funnel?: string;
  step?: string;
}

// Recordings: find the sessions of a person, of an event, or of the people who
// dropped off a funnel step; the latest ones otherwise. One action: find.
export default async function ReplaysPage({ searchParams }: { searchParams: Params }) {
  const ctx = await a3Context(searchParams.app);
  if (!liveModules().replay) {
    return (
      <A3Shell title="Arriving" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/replay">
        <Notice text={ARRIVING} />
      </A3Shell>
    );
  }
  const q = appQuery(ctx.app);
  let replays: Replay[] = [];
  let offset: number | null = null;
  let heading = "Latest recordings";
  let problem: string | null = null;
  let recording: boolean | null = null;
  let funnels: { id: string; name: string }[] = [];
  if (ctx.api && ctx.insights) {
    try {
      const [settings, saved] = await Promise.all([ctx.api.recordingSettings(), ctx.insights.saved()]);
      recording = settings.settings.replay_enabled;
      funnels = saved.insights.filter((i) => i.kind === "funnels").map((i) => ({ id: i.id, name: i.name }));
      if (searchParams.funnel && searchParams.step) {
        const step = Math.max(2, Math.min(20, Number(searchParams.step) || 2));
        const insight = (await ctx.insights.savedOne(searchParams.funnel)).insight;
        const people = (await ctx.insights.funnelPeople(insight.query, step, true)).people;
        replays = people.length ? (await ctx.api.replaysForPeople(people.map((p) => p.id))).replays : [];
        heading = `Recordings of people who dropped off before step ${step} of ${insight.name}`;
      } else if (searchParams.event) {
        const r = await ctx.api.replays({ event: searchParams.event.trim() });
        replays = r.replays;
        offset = r.offset_ms ?? null;
        heading = "The recording of this event";
      } else if (searchParams.person) {
        replays = (await ctx.api.replays({ distinct_id: searchParams.person.trim() })).replays;
        heading = `Recordings of ${searchParams.person.trim()}`;
      } else {
        replays = (await ctx.api.replays()).replays;
      }
    } catch (e) {
      problem = problemText(e);
    }
  }
  const watch = (r: Replay) => `/product-analytics/replay/${encodeURIComponent(r.session_id)}?${q}${offset ? `&t=${offset}` : ""}`;
  return (
    <A3Shell title="Recordings" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/replay">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.api ? (
        <>
          <section className="card">
            <form method="get" className="row" style={{ alignItems: "end" }}>
              <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
              <label style={{ display: "grid", gap: 4, fontSize: 13 }}>
                <span className="muted">Person (any of their ids)</span>
                <input name="person" defaultValue={searchParams.person ?? ""} placeholder="buyer@shop.example" />
              </label>
              <label style={{ display: "grid", gap: 4, fontSize: 13 }}>
                <span className="muted">or an event id</span>
                <input name="event" defaultValue={searchParams.event ?? ""} placeholder="0192a0a0-..." />
              </label>
              <button className="primary" type="submit">
                Find recordings
              </button>
            </form>
            {funnels.length ? (
              <form method="get" className="row" style={{ alignItems: "end", marginTop: 12 }}>
                <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
                <label style={{ display: "grid", gap: 4, fontSize: 13 }}>
                  <span className="muted">People who dropped off in funnel</span>
                  <select name="funnel" defaultValue={searchParams.funnel ?? ""}>
                    {funnels.map((f) => (
                      <option key={f.id} value={f.id}>
                        {f.name}
                      </option>
                    ))}
                  </select>
                </label>
                <label style={{ display: "grid", gap: 4, fontSize: 13 }}>
                  <span className="muted">before step</span>
                  <input name="step" type="number" min={2} max={20} defaultValue={searchParams.step ?? "2"} style={{ width: 70 }} />
                </label>
                <button type="submit">Show their recordings</button>
              </form>
            ) : null}
            <p className="muted" data-copy-source="product-analytics /recording-settings (live)">
              {recording === null ? "" : recording ? "Recording is on for this app. " : "Recording is off for this app. "}
              <a href={`/product-analytics/replay/settings?${q}`}>Recording and click map settings</a>
            </p>
            <Problem text={problem} />
          </section>
          <section className="card">
            <h2>{heading}</h2>
            {replays.length === 0 ? (
              <p className="muted">No recordings match yet.</p>
            ) : (
              <table>
                <thead>
                  <tr>
                    <th>Started</th>
                    <th>Length</th>
                    <th>Person</th>
                    <th>Clicks</th>
                    <th>First page</th>
                    <th />
                  </tr>
                </thead>
                <tbody data-copy-source="product-analytics /replays (live query)">
                  {replays.map((r) => (
                    <tr key={r.session_id}>
                      <td>{fmt(r.started_at)}</td>
                      <td>{duration(r.duration_ms)}</td>
                      <td>{r.distinct_id}</td>
                      <td>{r.click_count}</td>
                      <td>{r.urls[0] ?? ""}</td>
                      <td>
                        <a href={watch(r)}>Watch</a>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>
        </>
      ) : null}
    </A3Shell>
  );
}
