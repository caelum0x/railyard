// Site entries for the Insights edge collector (RYP-E1, Wave P PY1.3).
//
// The collector reads one KV entry per app, key `site:<site_key>`:
// `{ org_id, app_id, events_limit, paused }` (contract
// apps/insights-edge/contract/INGEST-API.md). The console writes it when an
// app is created and again whenever the org's plan, add-on or pause changes,
// so caps and pauses reach the edge within the collector's 60 s cache.

import { access } from "./ladder.ts";
import type { Repo } from "./repo.ts";
import type { App } from "./types.ts";

export interface SiteEntry {
  org_id: string;
  app_id: string;
  events_limit: number;
  paused: boolean;
}

export interface SitesStore {
  put(siteKey: string, entry: SiteEntry): Promise<void>;
}

export class MemorySites implements SitesStore {
  entries = new Map<string, SiteEntry>();
  async put(siteKey: string, entry: SiteEntry) {
    this.entries.set(`site:${siteKey}`, structuredClone(entry));
  }
}

/** The Workers KV namespace bound as SITES (wrangler.jsonc), through the OpenNext context. */
export function kvSites(): SitesStore {
  return {
    async put(siteKey, entry) {
      const { getCloudflareContext } = await import("@opennextjs/cloudflare");
      const { env } = await getCloudflareContext({ async: true });
      const kv = (env as unknown as { SITES?: { put(key: string, value: string): Promise<void> } }).SITES;
      if (!kv) throw new Error("the SITES namespace is not bound to the console");
      await kv.put(`site:${siteKey}`, JSON.stringify(entry));
    },
  };
}

/** The entry for one app from the org's current entitlement. */
export async function siteEntry(repo: Repo, app: Pick<App, "id" | "org_id">, now: Date): Promise<SiteEntry> {
  const a = access(await repo.getEntitlement(app.org_id), now);
  return { org_id: app.org_id, app_id: app.id, events_limit: a.eventsLimit, paused: a.state === "paused" || !a.canAddApps };
}

export async function registerSite(sites: SitesStore, repo: Repo, app: App, now: Date): Promise<void> {
  await sites.put(app.site_key, await siteEntry(repo, app, now));
}

/** Rewrites every app's entry of the org (after a plan, add-on or pause change). */
export async function syncSites(sites: SitesStore, repo: Repo, orgId: string, now: Date): Promise<number> {
  const apps = await repo.listApps(orgId);
  for (const app of apps) await registerSite(sites, repo, app, now);
  return apps.length;
}
