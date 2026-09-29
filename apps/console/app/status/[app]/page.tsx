import { notFound } from "next/navigation";
import { deps } from "../../../lib/deps.ts";
import { statusView } from "../../../lib/watch/service.ts";
import { SLUG } from "../../../lib/watch/types.ts";

// A public status page per app (Wave P PY2.2, RYP-E4): live results only,
// the check's name and never its address, host or any vendor name.
export const revalidate = 30;

export async function generateMetadata({ params }: { params: { app: string } }) {
  const check = deps().watch.live && SLUG.test(params.app) ? await deps().watch.store.findCheckBySlug(params.app) : null;
  return { title: check ? `${check.name} status` : "Status", robots: { index: false } };
}

export default async function StatusPage({ params }: { params: { app: string } }) {
  const { watch } = deps();
  if (!watch.live || !SLUG.test(params.app)) notFound();
  const check = await watch.store.findCheckBySlug(params.app);
  if (!check || check.paused) notFound();
  const view = statusView(check, await watch.store.getStatus(check.id));
  const tone = view.state === "up" ? "ok" : view.state === "down" ? "error" : "muted";
  return (
    <main className="main" style={{ maxWidth: 720, margin: "0 auto" }}>
      <h1>{view.name}</h1>
      <section className="card">
        <h2 className={tone} data-copy-source="railyard watch_status (live results)">
          {view.label}
        </h2>
        {view.uptime ? <p data-copy-source="railyard watch_status (live results)">{view.uptime}</p> : null}
        {view.lastChecked ? <p className="muted">Last checked {view.lastChecked}</p> : <p className="muted">The first check runs within two minutes.</p>}
        {view.bars.length ? (
          <div role="img" aria-label={`Last ${view.bars.length} checks`} style={{ display: "flex", gap: 2, marginTop: 12 }}>
            {view.bars.map((b) => (
              <span key={b.at} title={`${b.at.slice(0, 16).replace("T", " ")} UTC: ${b.ok ? "up" : "down"}`} style={{ flex: 1, height: 28, borderRadius: 2, background: b.ok ? "var(--ok, #2f9e6b)" : "var(--error, #c84a4a)" }} />
            ))}
          </div>
        ) : null}
      </section>
    </main>
  );
}
