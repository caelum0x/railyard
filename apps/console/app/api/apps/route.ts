import { addApp, AppError, installSnippet } from "../../../lib/apps.ts";
import { sessionFromRequest } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { json, sameOrigin } from "../../../lib/http.ts";
import { registerSite } from "../../../lib/sites.ts";

export async function POST(req: Request) {
  const { cfg, repo, sites } = deps();
  const s = await sessionFromRequest(req);
  if (!s) return json({ error: "Sign in again." }, 401);
  if (!sameOrigin(req, cfg.consoleUrl)) return json({ error: "Forbidden" }, 403);
  const body = (await req.json().catch(() => ({}))) as { name?: unknown };
  if (typeof body.name !== "string") return json({ error: "Give the app a name." }, 400);
  try {
    const now = new Date();
    const app = await addApp(repo, s.org, body.name, now);
    // The edge collector needs the site entry to accept events (RYP-E1).
    const siteReady = await registerSite(sites, repo, app, now).then(
      () => true,
      (err: Error) => {
        console.error(`site entry for app ${app.id} of org ${s.org} failed: ${err.message}`);
        return false;
      },
    );
    return json({ app: { id: app.id, name: app.name }, snippet: installSnippet(app, cfg.insightsUrl), site_ready: siteReady }, 201);
  } catch (err) {
    if (err instanceof AppError) return json({ error: err.message, code: err.code }, err.code === "no_plan" ? 402 : 409);
    console.error(`add app failed for org ${s.org}: ${(err as Error).message}`);
    return json({ error: "Could not add the app. Try again." }, 500);
  }
}
