// Downloads a SQL answer as CSV: the session decides the org, the service
// runs the query with the same guards as the page.
import { sessionFromRequest } from "../../../../../lib/current.ts";
import { deps } from "../../../../../lib/deps.ts";
import { paConfig, pickApp } from "../../_lib/http.ts";
import { insightsApi } from "../../_lib/insights-client.ts";

export const dynamic = "force-dynamic";

const json = (status: number, error: string) => new Response(JSON.stringify({ error }), { status, headers: { "content-type": "application/json" } });

export async function POST(req: Request) {
  const s = await sessionFromRequest(req);
  if (!s) return json(401, "Sign in again.");
  const form = await req.formData();
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, String(form.get("app") ?? ""));
  const cfg = paConfig();
  if (!app || !cfg) return json(503, "Product analytics is arriving on this deployment.");
  const sql = String(form.get("sql") ?? "").slice(0, 20_000);
  const res = await insightsApi(cfg, { org_id: s.org, app_ids: apps.map((a) => a.id) }, app.id).sqlCsv(sql);
  if (!res.ok) return new Response(await res.text(), { status: res.status, headers: { "content-type": "application/json" } });
  return new Response(res.body, { status: 200, headers: { "content-type": "text/csv; charset=utf-8", "content-disposition": 'attachment; filename="query.csv"', "cache-control": "no-store" } });
}
