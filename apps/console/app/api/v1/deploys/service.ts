// Deploy records for `railyard deploy` and `railyard rollback` (RYP-D4).
// A token only ever reads and writes its own org's deploys.

import { addApp, AppError, normaliseAppName, pausedMessage } from "../../../../lib/apps.ts";
import { access } from "../../../../lib/ladder.ts";
import type { Repo } from "../../../../lib/repo.ts";
import type { Deploy } from "../../../../lib/types.ts";

const FRAMEWORKS = ["nextjs", "vite", "astro", "static"] as const;
const STATUSES = ["running", "succeeded", "failed"] as const;
const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export interface DeployInput {
  app: string;
  url: string;
  commit_sha: string | null;
  branch: string;
  framework: Deploy["framework"];
  version_id: string | null;
  duration_ms: number | null;
  status: "running" | "succeeded" | "failed";
  rollback_of: string | null;
}

export function parseDeployInput(body: unknown): DeployInput | string {
  if (!body || typeof body !== "object") return "invalid body";
  const b = body as Record<string, unknown>;
  const s = (k: string, max: number) => (typeof b[k] === "string" && (b[k] as string).length <= max ? (b[k] as string) : null);
  const app = s("app", 60);
  const branch = s("branch", 200);
  if (!app || !branch) return "app and branch are required";
  const url = s("url", 300) ?? "";
  if (url && !/^https:\/\/[a-z0-9.-]+(\/|$)/i.test(url)) return "url must be https";
  const framework = b.framework as Deploy["framework"];
  if (!FRAMEWORKS.includes(framework)) return "unsupported framework";
  const status = b.status as DeployInput["status"];
  if (!STATUSES.includes(status)) return "invalid status";
  const commit = s("commit_sha", 64);
  if (commit && !/^[0-9a-f]{7,64}$/i.test(commit)) return "invalid commit";
  const duration = typeof b.duration_ms === "number" && Number.isInteger(b.duration_ms) && b.duration_ms >= 0 ? b.duration_ms : null;
  return { app, url, commit_sha: commit, branch, framework, version_id: s("version_id", 64), duration_ms: duration, status, rollback_of: s("rollback_of", 64) };
}

export async function recordDeploy(repo: Repo, orgId: string, input: DeployInput, now: Date): Promise<{ status: number; body: Record<string, unknown> }> {
  const a = access(await repo.getEntitlement(orgId), now);
  const paused = pausedMessage(a);
  if (paused) return { status: 402, body: { error: paused } };
  // The CLI sends the folder name when --app is not given ("My Shop"); apps are
  // stored under the normalised name ("my-shop"), so look up that one.
  let app = await repo.findAppByName(orgId, normaliseAppName(input.app));
  if (!app) {
    try {
      app = await addApp(repo, orgId, input.app, now);
    } catch (e) {
      if (e instanceof AppError) return { status: e.code === "no_plan" ? 402 : 409, body: { error: e.message } };
      throw e;
    }
  }
  if (input.rollback_of && (!UUID.test(input.rollback_of) || !(await repo.getDeploy(orgId, input.rollback_of)))) return { status: 404, body: { error: "rollback target not found" } };
  const d = await repo.createDeploy({
    org_id: orgId,
    app_id: app.id,
    url: input.url,
    commit_sha: input.commit_sha,
    branch: input.branch,
    framework: input.framework,
    version_id: input.version_id,
    duration_ms: input.duration_ms,
    status: input.status,
    rollback_of: input.rollback_of,
  });
  return { status: 201, body: { deploy: publicDeploy(d, app.name) } };
}

export async function listDeploys(repo: Repo, orgId: string, appName: string | null, branch: string | null) {
  const apps = await repo.listApps(orgId);
  const wanted = appName ? normaliseAppName(appName) : null;
  const app = wanted ? apps.find((a) => a.name === wanted) : null;
  if (appName && !app) return { status: 200, body: { deploys: [] } };
  const names = new Map(apps.map((a) => [a.id, a.name]));
  const rows = (await repo.listDeploys(orgId, app?.id)).filter((d) => !branch || d.branch === branch);
  return { status: 200, body: { deploys: rows.map((d) => publicDeploy(d, names.get(d.app_id) ?? "")) } };
}

export function publicDeploy(d: Deploy, app: string) {
  return {
    id: d.id,
    app,
    url: d.url,
    commit_sha: d.commit_sha,
    branch: d.branch,
    framework: d.framework,
    version_id: d.version_id,
    duration_ms: d.duration_ms,
    status: d.status,
    rollback_of: d.rollback_of,
    created_at: d.created_at,
  };
}
