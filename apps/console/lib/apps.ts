// Apps: create one (plan limits enforced) and the install snippet it shows.

import { access } from "./ladder.ts";
import { randomToken } from "./crypto.ts";
import type { Repo } from "./repo.ts";
import type { App } from "./types.ts";

export const APP_NAME = /^[a-z0-9][a-z0-9-]{0,39}$/;

export class AppError extends Error {
  code: string;
  constructor(code: string, message: string) {
    super(message);
    this.code = code;
  }
}

export function normaliseAppName(raw: string): string {
  return raw.trim().toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);
}

export async function addApp(repo: Repo, orgId: string, rawName: string, now: Date): Promise<App> {
  const name = normaliseAppName(rawName);
  if (!APP_NAME.test(name)) throw new AppError("invalid_name", "Use letters, numbers and dashes.");
  const a = access(await repo.getEntitlement(orgId), now);
  if (!a.canAddApps) {
    throw new AppError("no_plan", a.state === "paused" ? "Railyard is paused. Resume it on the Billing screen." : "Add a card on the Billing screen first ($1, credited back).");
  }
  const apps = await repo.listApps(orgId);
  if (apps.length >= a.appsLimit) throw new AppError("limit", `Your plan includes ${a.appsLimit} apps.`);
  if (apps.some((x) => x.name === name)) throw new AppError("exists", `You already have an app called ${name}.`);
  return repo.createApp({ orgId, name, siteKey: `ry_site_${randomToken(18)}` });
}

export interface Snippet {
  script: string;
  deploy: string;
}

/** What the user pastes: one script tag for Insights and one deploy command. The site key is public by design. */
export function installSnippet(app: Pick<App, "name" | "site_key">, insightsUrl: string): Snippet {
  return {
    script: `<script defer src="${insightsUrl}/s.js" data-site="${app.site_key}"></script>`,
    deploy: `npx railyard deploy --app ${app.name}`,
  };
}
