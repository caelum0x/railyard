// Who is asking and for which app, for every insights screen and action.
import { redirect } from "next/navigation";
import { currentSession } from "../../../../../lib/current.ts";
import { deps } from "../../../../../lib/deps.ts";
import { paConfig, pickApp, type AppRef } from "../../_lib/http.ts";
import { insightsApi } from "../../_lib/insights-client.ts";

export interface InsightsContext {
  apps: AppRef[];
  names: string[];
  app: AppRef | null;
  api: ReturnType<typeof insightsApi> | null;
}

export async function insightsContext(appName: string | undefined | null): Promise<InsightsContext> {
  const s = await currentSession();
  if (!s) redirect("/");
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, appName ?? undefined);
  const cfg = paConfig();
  const api = app && cfg ? insightsApi(cfg, { org_id: s.org, app_ids: apps.map((a) => a.id) }, app.id) : null;
  return { apps, names: apps.map((a) => a.name), app, api };
}

/** `?app=` for links that keep the chosen app. */
export const appQuery = (app: AppRef | null) => (app ? `app=${encodeURIComponent(app.name)}` : "");
