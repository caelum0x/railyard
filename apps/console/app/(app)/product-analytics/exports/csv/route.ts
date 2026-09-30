// Downloads a saved insight's answer as CSV: the session decides the org, the
// service computes (through its cache) and writes the file.
import { sessionFromRequest } from "../../../../../lib/current.ts";
import { deps } from "../../../../../lib/deps.ts";
import { a3Api } from "../../_lib/a3-client.ts";
import { paConfig, pickApp } from "../../_lib/http.ts";

export const dynamic = "force-dynamic";

const json = (status: number, error: string) => new Response(JSON.stringify({ error }), { status, headers: { "content-type": "application/json" } });

export async function GET(req: Request) {
  const s = await sessionFromRequest(req);
  if (!s) return json(401, "Sign in again.");
  const url = new URL(req.url);
  const insight = url.searchParams.get("insight") ?? "";
  if (!/^[0-9a-f-]{36}$/.test(insight)) return json(400, "Choose a saved insight.");
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, url.searchParams.get("app") ?? undefined);
  const cfg = paConfig();
  if (!app || !cfg) return json(503, "Product analytics is arriving on this deployment.");
  const res = await a3Api(cfg, { org_id: s.org, app_ids: apps.map((a) => a.id) }, app.id).savedInsightCsv(insight);
  if (!res.ok) return new Response(await res.text(), { status: res.status, headers: { "content-type": "application/json" } });
  return new Response(res.body, { status: 200, headers: { "content-type": "text/csv; charset=utf-8", "content-disposition": res.headers.get("content-disposition") ?? 'attachment; filename="insight.csv"', "cache-control": "no-store" } });
}
