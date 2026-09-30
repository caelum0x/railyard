import { redirect } from "next/navigation";
import { a3Context, appQuery, field, isRedirect } from "../_parts/context.ts";
import { A3Shell, NO_APP, NOT_CONNECTED, Notice, Problem } from "../_parts/A3Shell.tsx";
import { ARRIVING, liveModules } from "../_parts/live.ts";
import { problemText, type RecordingSettings } from "../../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

async function save(form: FormData) {
  "use server";
  if (!liveModules().replay) redirect("/product-analytics/replay");
  const ctx = await a3Context(field(form, "app", 200));
  if (!ctx.api) redirect("/product-analytics/replay/settings");
  const back = `/product-analytics/replay/settings?${appQuery(ctx.app)}`;
  try {
    await ctx.api.saveRecordingSettings({
      replay_enabled: form.get("replay_enabled") === "on",
      heatmaps_enabled: form.get("heatmaps_enabled") === "on",
      replay_sample_rate: Math.min(100, Math.max(0, Number(field(form, "sample", 10)) || 0)) / 100,
      replay_min_duration_ms: Math.min(30, Math.max(0, Math.round(Number(field(form, "min_seconds", 10)) || 0))) * 1000,
    });
  } catch (e) {
    if (isRedirect(e)) throw e;
    redirect(`${back}&problem=${encodeURIComponent(problemText(e))}`);
  }
  redirect(`${back}&saved=1`);
}

// Recording and click map switches for one app. One action: save.
export default async function RecordingSettingsPage({ searchParams }: { searchParams: { app?: string; saved?: string; problem?: string } }) {
  const ctx = await a3Context(searchParams.app);
  if (!liveModules().replay) {
    return (
      <A3Shell title="Arriving" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/replay">
        <Notice text={ARRIVING} />
      </A3Shell>
    );
  }
  let s: RecordingSettings | null = null;
  let store: string | null = null;
  let days = 0;
  let problem: string | null = searchParams.problem ?? null;
  if (ctx.api) {
    try {
      const r = await ctx.api.recordingSettings();
      s = r.settings;
      store = r.store;
      days = r.retention_days;
    } catch (e) {
      problem = problemText(e);
    }
  }
  return (
    <A3Shell title="Recording settings" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/replay">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {s ? (
        <section className="card">
          <form action={save} style={{ display: "grid", gap: 12, maxWidth: 560 }}>
            <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
            <label>
              <input type="checkbox" name="replay_enabled" defaultChecked={s.replay_enabled} disabled={!store} /> Record sessions
            </label>
            <label style={{ display: "grid", gap: 4, fontSize: 13 }}>
              <span className="muted">Share of sessions recorded (percent)</span>
              <input name="sample" type="number" min={0} max={100} step={1} defaultValue={Math.round(s.replay_sample_rate * 100)} style={{ width: 90 }} />
            </label>
            <label style={{ display: "grid", gap: 4, fontSize: 13 }}>
              <span className="muted">Skip sessions shorter than (seconds)</span>
              <input name="min_seconds" type="number" min={0} max={30} defaultValue={Math.round(s.replay_min_duration_ms / 1000)} style={{ width: 90 }} />
            </label>
            <label>
              <input type="checkbox" name="heatmaps_enabled" defaultChecked={s.heatmaps_enabled} /> Collect click maps and scroll depth
            </label>
            <p className="muted" data-copy-source="product-analytics src/replay/limits.ts and src/remote-config/sections.ts">
              Every typed value is masked in the app and again before a recording is stored, text marked data-private is masked and data-block areas are left out. Recordings are kept {days} days and stop after 30 minutes.
              {store ? "" : " Recording needs object storage, which is not set up on this deployment yet."}
            </p>
            <div>
              <button className="primary" type="submit">
                Save
              </button>
            </div>
          </form>
          {searchParams.saved ? <p>Saved. Apps pick this up the next time they load.</p> : null}
          <Problem text={problem} />
        </section>
      ) : (
        <Problem text={problem} />
      )}
    </A3Shell>
  );
}
