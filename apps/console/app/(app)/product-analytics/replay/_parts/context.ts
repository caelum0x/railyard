// Who is asking and for which app, for the recordings, click maps, surveys,
// experiments and exports screens and their actions.
import { redirect } from "next/navigation";
import { currentSession } from "../../../../../lib/current.ts";
import { deps } from "../../../../../lib/deps.ts";
import { a3Api, type A3Api } from "../../_lib/a3-client.ts";
import { paConfig, pickApp, type AppRef, type Scope } from "../../_lib/http.ts";
import { insightsApi } from "../../_lib/insights-client.ts";

export interface A3Context {
  org: string;
  email: string;
  names: string[];
  app: AppRef | null;
  scope: Scope;
  api: A3Api | null;
  insights: ReturnType<typeof insightsApi> | null;
}

export async function a3Context(appName: string | undefined | null): Promise<A3Context> {
  const s = await currentSession();
  if (!s) redirect("/");
  const apps = await deps().repo.listApps(s.org);
  const app = pickApp(apps, appName ?? undefined);
  const cfg = paConfig();
  const scope: Scope = { org_id: s.org, app_ids: apps.map((a) => a.id) };
  return {
    org: s.org,
    email: s.email,
    names: apps.map((a) => a.name),
    app,
    scope,
    api: app && cfg ? a3Api(cfg, scope, app.id) : null,
    insights: app && cfg ? insightsApi(cfg, scope, app.id) : null,
  };
}

export const appQuery = (app: AppRef | null): string => (app ? `app=${encodeURIComponent(app.name)}` : "");

/** Next's redirect() throws a marked error; let it pass through catch blocks. */
export const isRedirect = (e: unknown): boolean => !!e && typeof e === "object" && "digest" in e;

export const field = (form: FormData, name: string, max = 2000): string => String(form.get(name) ?? "").slice(0, max);

export const formObject = (form: FormData): Record<string, string> => {
  const out: Record<string, string> = {};
  form.forEach((v, k) => {
    if (typeof v === "string") out[k] = v.slice(0, 5000);
  });
  return out;
};
