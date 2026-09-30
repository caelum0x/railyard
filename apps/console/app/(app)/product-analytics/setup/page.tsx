import { redirect } from "next/navigation";
import { currentSession } from "../../../../lib/current.ts";
import { deps } from "../../../../lib/deps.ts";
import { Shell } from "../_components/Shell.tsx";
import { fmtTime, paConfig, paFetch, PaError, pickApp, type Settings, type Status } from "../_lib/http.ts";

export const dynamic = "force-dynamic";

const HOST_DEFAULT = "https://pa.railyard.ereboslab.com";

async function saveSettings(form: FormData) {
  "use server";
  const s = await currentSession();
  if (!s) redirect("/");
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, String(form.get("app") ?? ""));
  const cfg = paConfig();
  if (!app || !cfg) redirect("/product-analytics/setup");
  const key = String(form.get("flags_client_key") ?? "").trim();
  await paFetch(cfg, { org_id: s.org, app_ids: apps.map((a) => a.id) }, `/v1/projects/${app.id}/settings`, {
    method: "PUT",
    body: JSON.stringify({
      require_consent: form.get("require_consent") === "on",
      autocapture: form.get("autocapture") === "on",
      capture_pageview: form.get("capture_pageview") === "on",
      flags_client_key: key || null,
    }),
  });
  redirect(`/product-analytics/setup?app=${encodeURIComponent(app.name)}&saved=1`);
}

// Setup: the two lines the app needs, then proof that its first event arrived.
export default async function SetupPage({ searchParams }: { searchParams: { app?: string; saved?: string } }) {
  const s = (await currentSession())!;
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, searchParams.app);
  const cfg = paConfig();
  const host = process.env.PRODUCT_ANALYTICS_PUBLIC_URL?.trim() || HOST_DEFAULT;
  let status: Status | null = null;
  let settings: Settings | null = null;
  let problem: string | null = null;
  if (app && cfg) {
    const scope = { org_id: s.org, app_ids: apps.map((a) => a.id) };
    try {
      [status, { settings }] = await Promise.all([
        paFetch<Status>(cfg, scope, `/v1/projects/${app.id}/status`),
        paFetch<{ settings: Settings }>(cfg, scope, `/v1/projects/${app.id}/settings`),
      ]);
    } catch (e) {
      problem = e instanceof PaError ? e.message : "Product analytics could not be reached. Try again shortly.";
    }
  }
  return (
    <Shell title="Set up product analytics" apps={apps.map((a) => a.name)} app={app?.name ?? null} live={!!status && !status.first_event}>
      {!app ? (
        <section className="card">
          <p className="muted">Add an app first.</p>
          <a href="/apps">Add an app</a>
        </section>
      ) : (
        <>
          <section className="card">
            <h2>1. Add the package</h2>
            <pre>
              <code>npm install @railyard/analytics-js</code>
            </pre>
            <p className="muted">Scribase projects already have these lines after scribase add analytics.</p>
            <pre>
              <code data-copy-source="railyard.apps.site_key">{`NEXT_PUBLIC_ANALYTICS_SITE=${app.site_key}\nNEXT_PUBLIC_ANALYTICS_HOST=${host}`}</code>
            </pre>
            <p className="muted">Expo uses EXPO_PUBLIC_ and Vite uses VITE_ in front of the same names.</p>
          </section>
          <section className="card">
            <h2>2. Send the first event</h2>
            <pre>
              <code>{`import { createAnalytics } from "@railyard/analytics-js";\n\nconst analytics = createAnalytics();\nanalytics.setConsent("granted"); // from your consent banner\nanalytics.capture("signed_up");`}</code>
            </pre>
            {problem ? <p>{problem}</p> : null}
            {!cfg ? <p className="muted">Product analytics is arriving on this deployment.</p> : null}
            {status?.first_event ? (
              <p data-copy-source="product-analytics /v1/projects/{id}/status (live query)">
                Done: {status.first_event.event} arrived at {fmtTime(status.first_event.at)}.
              </p>
            ) : status ? (
              <p className="muted">Waiting for the first event. This screen checks every 10 seconds.</p>
            ) : null}
          </section>
          {settings ? (
            <section className="card">
              <h2>Settings</h2>
              {searchParams.saved ? <p className="muted">Saved.</p> : null}
              <form action={saveSettings}>
                <input type="hidden" name="app" value={app.name} />
                <p>
                  <label>
                    <input type="checkbox" name="require_consent" defaultChecked={settings.require_consent} /> Ask for consent before sending
                  </label>
                </p>
                <p>
                  <label>
                    <input type="checkbox" name="autocapture" defaultChecked={settings.autocapture} /> Record clicks automatically
                  </label>
                </p>
                <p>
                  <label>
                    <input type="checkbox" name="capture_pageview" defaultChecked={settings.capture_pageview} /> Record page views
                  </label>
                </p>
                <p>
                  <label>
                    Flags key of this app&apos;s environment <input name="flags_client_key" defaultValue={settings.flags_client_key ?? ""} placeholder="ry_flags_..." />
                  </label>
                </p>
                <button type="submit">Save</button>
              </form>
            </section>
          ) : null}
        </>
      )}
    </Shell>
  );
}
