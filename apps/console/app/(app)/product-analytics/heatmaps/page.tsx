import { a3Context, appQuery } from "../replay/_parts/context.ts";
import { A3Shell, NO_APP, NOT_CONNECTED, Notice, Problem, ShareBar } from "../replay/_parts/A3Shell.tsx";
import { ARRIVING, liveModules } from "../replay/_parts/live.ts";
import { PATHS, problemText, type HeatmapAnswer } from "../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

const TYPES = [
  { value: "click", label: "Clicks" },
  { value: "rageclick", label: "Rage clicks" },
  { value: "deadclick", label: "Clicks that did nothing" },
  { value: "mousemove", label: "Mouse rests" },
];
const RANGES = [
  { value: "1", label: "Last day" },
  { value: "7", label: "Last 7 days" },
  { value: "30", label: "Last 30 days" },
  { value: "90", label: "Last 90 days" },
];

// Click map of one page: the page in a frame with the clicks on top, and how
// far people scrolled. One action: show the map.
export default async function HeatmapsPage({ searchParams }: { searchParams: { app?: string; url?: string; type?: string; days?: string } }) {
  const ctx = await a3Context(searchParams.app);
  if (!liveModules().replay) {
    return (
      <A3Shell title="Arriving" names={ctx.names} app={ctx.app?.name ?? null} current={PATHS.heat}>
        <Notice text={ARRIVING} />
      </A3Shell>
    );
  }
  const type = TYPES.some((t) => t.value === searchParams.type) ? searchParams.type! : "click";
  const days = RANGES.some((r) => r.value === searchParams.days) ? Number(searchParams.days) : 7;
  let urls: { url: string; clicks: number }[] = [];
  let answer: HeatmapAnswer | null = null;
  let link: string | null = null;
  let collecting: boolean | null = null;
  let problem: string | null = null;
  if (ctx.api) {
    try {
      const [u, s] = await Promise.all([ctx.api.heatmapUrls(), ctx.api.recordingSettings()]);
      urls = u.urls;
      collecting = s.settings.heatmaps_enabled;
      const url = searchParams.url?.trim() || urls[0]?.url;
      if (url) {
        const from = new Date(Date.now() - days * 86_400_000).toISOString();
        [answer, { url: link }] = await Promise.all([ctx.api.heatmap({ url, type, from }), ctx.api.heatmapLink({ url, type, from, width: 1280 })]);
      }
    } catch (e) {
      problem = problemText(e);
    }
  }
  const selected = searchParams.url?.trim() || urls[0]?.url || "";
  return (
    <A3Shell title="Click maps" names={ctx.names} app={ctx.app?.name ?? null} current={PATHS.heat}>
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.api ? (
        <section className="card">
          <form method="get" className="row" style={{ alignItems: "end" }}>
            <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
            <label style={{ display: "grid", gap: 4, fontSize: 13, flex: 1 }}>
              <span className="muted">Page address</span>
              <input name="url" list="heat-urls" defaultValue={selected} placeholder="https://shop.example/pricing" />
              <datalist id="heat-urls">
                {urls.map((u) => (
                  <option key={u.url} value={u.url} />
                ))}
              </datalist>
            </label>
            <label style={{ display: "grid", gap: 4, fontSize: 13 }}>
              <span className="muted">Show</span>
              <select name="type" defaultValue={type}>
                {TYPES.map((t) => (
                  <option key={t.value} value={t.value}>
                    {t.label}
                  </option>
                ))}
              </select>
            </label>
            <label style={{ display: "grid", gap: 4, fontSize: 13 }}>
              <span className="muted">Period</span>
              <select name="days" defaultValue={String(days)}>
                {RANGES.map((r) => (
                  <option key={r.value} value={r.value}>
                    {r.label}
                  </option>
                ))}
              </select>
            </label>
            <button className="primary" type="submit">
              Show map
            </button>
          </form>
          {collecting === false ? (
            <p className="muted">
              Click maps are off for this app. <a href={`/product-analytics/replay/settings?${appQuery(ctx.app)}`}>Turn them on</a>.
            </p>
          ) : null}
          <Problem text={problem} />
        </section>
      ) : null}
      {answer && link ? (
        <>
          <section className="card">
            <p className="muted" data-copy-source="product-analytics click map API (live query)">
              {answer.total.toLocaleString("en-US")} {TYPES.find((t) => t.value === type)!.label.toLowerCase()} on this page. The map link works for 10 minutes without signing in.
            </p>
            <iframe title="Click map" src={link} style={{ width: "100%", height: "80vh", border: "1px solid #d9dde5", borderRadius: 8 }} sandbox="allow-scripts allow-same-origin" referrerPolicy="no-referrer" />
          </section>
          <section className="card">
            <h2>How far people scrolled</h2>
            {answer.scroll.views === 0 ? (
              <p className="muted">No page views with scroll depth yet.</p>
            ) : (
              <div style={{ display: "grid", gap: 6 }} data-copy-source="product-analytics click map API, scroll (live query)">
                {answer.scroll.bands.map((b) => (
                  <ShareBar key={b.percent} label={`Reached ${b.percent}%`} value={b.reached} total={answer!.scroll.views} />
                ))}
              </div>
            )}
          </section>
        </>
      ) : null}
    </A3Shell>
  );
}
