// Download one person's data as one JSON file (proxied from the service with
// a fresh org token; the session decides the org).
import { sessionFromRequest } from "../../../../../../lib/current.ts";
import { deps } from "../../../../../../lib/deps.ts";
import { paConfig, paRaw, pickApp } from "../../../_lib/http.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request, { params }: { params: { distinct_id: string } }) {
  const s = await sessionFromRequest(req);
  if (!s) return new Response(JSON.stringify({ error: "Sign in again." }), { status: 401, headers: { "content-type": "application/json" } });
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, new URL(req.url).searchParams.get("app") ?? undefined);
  const cfg = paConfig();
  if (!app || !cfg) return new Response(JSON.stringify({ error: "Product analytics is arriving on this deployment." }), { status: 503, headers: { "content-type": "application/json" } });
  const res = await paRaw(cfg, { org_id: s.org, app_ids: apps.map((a) => a.id) }, `/v1/projects/${app.id}/persons/${encodeURIComponent(decodeURIComponent(params.distinct_id))}/export`);
  return new Response(res.body, {
    status: res.status,
    headers: { "content-type": "application/json", "content-disposition": 'attachment; filename="person-export.json"', "cache-control": "no-store" },
  });
}
