import { cookies } from "next/headers";
import { redirect } from "next/navigation";
import { a3Context, appQuery, formObject, isRedirect } from "../../replay/_parts/context.ts";
import { A3Shell, NO_APP, NOT_CONNECTED, Notice, Problem } from "../../replay/_parts/A3Shell.tsx";
import { destinationFromForm, problemText } from "../../_lib/a3-client.ts";

export const dynamic = "force-dynamic";

const SECRET_COOKIE = "pa_export_secret";

async function create(form: FormData) {
  "use server";
  const f = formObject(form);
  const ctx = await a3Context(f.app);
  if (!ctx.api) redirect("/product-analytics/exports");
  let id = "";
  try {
    const made = await ctx.api.createDestination(destinationFromForm(f));
    id = made.destination.id;
    if (made.signing_secret) {
      // Shown once on the destination's page, then gone: a short-lived cookie only this path reads.
      cookies().set(SECRET_COOKIE, JSON.stringify({ id, secret: made.signing_secret }), { httpOnly: true, secure: true, sameSite: "strict", maxAge: 300, path: "/product-analytics/exports" });
    }
  } catch (e) {
    if (isRedirect(e)) throw e;
    redirect(`/product-analytics/exports/new?${appQuery(ctx.app)}&problem=${encodeURIComponent(problemText(e))}`);
  }
  redirect(`/product-analytics/exports/${id}?${appQuery(ctx.app)}`);
}

const input = { display: "grid", gap: 4, fontSize: 13 } as const;

// A new destination: a webhook (the service signs every batch), an S3
// bucket (the service writes gzip NDJSON files), or Slack, Discord or HubSpot
// (QY1.4). One action: add it.
export default async function NewDestinationPage({ searchParams }: { searchParams: { app?: string; problem?: string } }) {
  const ctx = await a3Context(searchParams.app);
  return (
    <A3Shell title="New destination" names={ctx.names} app={ctx.app?.name ?? null} current="/product-analytics/exports">
      {!ctx.app ? <Notice text={NO_APP} /> : !ctx.api ? <Notice text={NOT_CONNECTED} /> : null}
      {ctx.api ? (
        <section className="card">
          <form action={create} style={{ display: "grid", gap: 12, maxWidth: 640 }}>
            <input type="hidden" name="app" value={ctx.app?.name ?? ""} />
            <label style={input}>
              <span className="muted">Name</span>
              <input name="name" required maxLength={200} placeholder="Warehouse" />
            </label>
            <fieldset style={{ display: "grid", gap: 8, border: "1px solid #d9dde5", borderRadius: 8, padding: 12 }}>
              <legend className="muted" style={{ fontSize: 13 }}>
                Send to
              </legend>
              <label>
                <input type="radio" name="kind" value="webhook" defaultChecked /> A webhook (HTTPS POST, signed)
              </label>
              <label style={input}>
                <span className="muted">Webhook URL</span>
                <input name="url" placeholder="https://hooks.shop.example/analytics" />
              </label>
              <label>
                <input type="radio" name="kind" value="s3" /> An S3-compatible bucket
              </label>
              <div className="row">
                <label style={input}>
                  <span className="muted">Endpoint</span>
                  <input name="endpoint" placeholder="https://s3.eu-central-1.amazonaws.com" />
                </label>
                <label style={input}>
                  <span className="muted">Bucket</span>
                  <input name="bucket" placeholder="analytics-lake" />
                </label>
                <label style={input}>
                  <span className="muted">Region</span>
                  <input name="region" placeholder="auto" />
                </label>
              </div>
              <div className="row">
                <label style={input}>
                  <span className="muted">Folder (optional)</span>
                  <input name="prefix" placeholder="railyard/events" />
                </label>
                <label style={input}>
                  <span className="muted">Access key id</span>
                  <input name="access_key_id" autoComplete="off" />
                </label>
                <label style={input}>
                  <span className="muted">Secret access key</span>
                  <input name="secret" type="password" autoComplete="off" />
                </label>
              </div>
              <label>
                <input type="radio" name="kind" value="slack" /> Slack (a message per batch)
              </label>
              <label>
                <input type="radio" name="kind" value="discord" /> Discord (a message per batch)
              </label>
              <label>
                <input type="radio" name="kind" value="hubspot" /> HubSpot (contacts kept up to date by email)
              </label>
              <div className="row">
                <label style={input}>
                  <span className="muted">Slack or Discord webhook address, or HubSpot private app token</span>
                  <input name="target" type="password" autoComplete="off" />
                </label>
                <label style={input}>
                  <span className="muted">Label (optional)</span>
                  <input name="label" maxLength={80} placeholder="#growth" />
                </label>
              </div>
            </fieldset>
            <label style={input}>
              <span className="muted">Only these events, comma separated (leave empty for all)</span>
              <input name="events" placeholder="signup, purchase" />
            </label>
            <label style={input}>
              <span className="muted">Start with</span>
              <select name="start" defaultValue="now">
                <option value="now">Events from now on</option>
                <option value="all">Every kept event, then new ones</option>
              </select>
            </label>
            <div>
              <button className="primary" type="submit">
                Add destination
              </button>
            </div>
          </form>
          <Problem text={searchParams.problem} />
        </section>
      ) : null}
    </A3Shell>
  );
}
