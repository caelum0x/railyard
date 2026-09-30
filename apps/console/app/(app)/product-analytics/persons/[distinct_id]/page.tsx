import { redirect } from "next/navigation";
import { currentSession } from "../../../../../lib/current.ts";
import { deps } from "../../../../../lib/deps.ts";
import { Shell } from "../../_components/Shell.tsx";
import { fmtTime, paConfig, paFetch, PaError, pickApp, type PersonRow } from "../../_lib/http.ts";

export const dynamic = "force-dynamic";

interface Detail {
  person: PersonRow;
  distinct_ids: string[];
  events: { uuid: string; event: string; timestamp: string; url: string | null }[];
  event_count: number;
}

async function requestDeletion(form: FormData) {
  "use server";
  const s = await currentSession();
  if (!s) redirect("/");
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, String(form.get("app") ?? ""));
  const cfg = paConfig();
  const id = String(form.get("distinct_id") ?? "");
  if (!app || !cfg || !id) redirect("/product-analytics/privacy");
  await paFetch(cfg, { org_id: s.org, app_ids: apps.map((a) => a.id) }, `/v1/projects/${app.id}/persons/${encodeURIComponent(id)}/delete`, { method: "POST" });
  redirect(`/product-analytics/privacy?app=${encodeURIComponent(app.name)}`);
}

// One person: their ids, properties and latest events; export or delete.
export default async function PersonPage({ params, searchParams }: { params: { distinct_id: string }; searchParams: { app?: string } }) {
  const s = (await currentSession())!;
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, searchParams.app);
  const cfg = paConfig();
  const distinctId = decodeURIComponent(params.distinct_id);
  let detail: Detail | null = null;
  let problem: string | null = null;
  if (app && cfg) {
    try {
      detail = await paFetch<Detail>(cfg, { org_id: s.org, app_ids: apps.map((a) => a.id) }, `/v1/projects/${app.id}/persons/${encodeURIComponent(distinctId)}`);
    } catch (e) {
      problem = e instanceof PaError ? e.message : "Product analytics could not be reached. Try again shortly.";
    }
  }
  const q = app ? `?app=${encodeURIComponent(app.name)}` : "";
  return (
    <Shell title="Person" apps={apps.map((a) => a.name)} app={app?.name ?? null}>
      {problem ? (
        <section className="card">
          <p>{problem}</p>
        </section>
      ) : null}
      {detail ? (
        <>
          <section className="card">
            <h2>{distinctId}</h2>
            <p className="muted" data-copy-source="product-analytics /v1/projects/{id}/persons/{distinct_id} (live query)">
              Also known as: {detail.distinct_ids.filter((d) => d !== distinctId).join(", ") || "no other ids"}. {detail.event_count.toLocaleString("en-US")} events.
            </p>
            <table>
              <tbody data-copy-source="product-analytics person properties (live query)">
                {Object.entries(detail.person.properties)
                  .filter(([k]) => !k.startsWith("$initial_"))
                  .slice(0, 40)
                  .map(([k, v]) => (
                    <tr key={k}>
                      <td>{k}</td>
                      <td>{typeof v === "string" ? v : JSON.stringify(v)}</td>
                    </tr>
                  ))}
              </tbody>
            </table>
          </section>
          <section className="card">
            <h2>Latest events</h2>
            <table>
              <tbody data-copy-source="product-analytics /v1/projects/{id}/persons/{distinct_id} (live query)">
                {detail.events.map((e) => (
                  <tr key={e.uuid}>
                    <td>{fmtTime(e.timestamp)}</td>
                    <td>{e.event}</td>
                    <td>{e.url ?? ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </section>
          <section className="card">
            <h2>Privacy</h2>
            <div className="row">
              <a href={`/product-analytics/persons/${encodeURIComponent(distinctId)}/export${q}`}>Download their data</a>
              <form action={requestDeletion}>
                <input type="hidden" name="app" value={app?.name ?? ""} />
                <input type="hidden" name="distinct_id" value={distinctId} />
                <button type="submit">Delete this person</button>
              </form>
            </div>
            <p className="muted">Deleting removes their events, ids and properties. The Privacy screen shows the request until it is done.</p>
          </section>
        </>
      ) : null}
    </Shell>
  );
}
