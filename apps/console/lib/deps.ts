// Server wiring: config, repository and payment client for route handlers
// and server components. Server-only (never imported by a "use client" file).

import { readConfig, type Config } from "./config.ts";
import { Dodo } from "./dodo.ts";
import { PostgrestRepo } from "./postgrest-repo.ts";
import type { Repo } from "./repo.ts";
import { insightsClient, type InsightsClient } from "./insights.ts";
import { kvSites, type SitesStore } from "./sites.ts";
import { PostgrestFlagsStore } from "./flags/store.ts";
import { TableFlagsBackend } from "./flags/table-backend.ts";
import { FlagsmithBackend } from "./flags/flagsmith-backend.ts";
import type { FlagsDeps } from "./flags/service.ts";
import { PostgrestWatchStore } from "./watch/store.ts";
import { noMailer, rallySend } from "./watch/mail.ts";
import type { WatchDeps } from "./watch/service.ts";

export interface Deps {
  cfg: Config;
  repo: Repo;
  dodo: Dodo;
  sites: SitesStore;
  insights: InsightsClient | null;
  flags: FlagsDeps;
  watch: WatchDeps;
}

function flagsDeps(cfg: Config): FlagsDeps {
  const store = new PostgrestFlagsStore({ url: cfg.scribaseUrl, serviceRoleKey: cfg.scribaseServiceRoleKey });
  const table = new TableFlagsBackend(store);
  const flagsmith = cfg.flags ? new FlagsmithBackend({ apiUrl: cfg.flags.apiUrl, adminToken: cfg.flags.adminToken }) : null;
  return { store, backend: flagsmith ?? table, backends: flagsmith ? { table, flagsmith } : { table } };
}

let cached: Deps | null = null;

export function deps(): Deps {
  if (cached) return cached;
  const cfg = readConfig();
  const repo = new PostgrestRepo({ url: cfg.scribaseUrl, serviceRoleKey: cfg.scribaseServiceRoleKey });
  cached = {
    cfg,
    repo,
    dodo: new Dodo({ base: cfg.dodoBase, apiKey: cfg.dodoApiKey, allowLive: cfg.dodoAllowLive }),
    sites: kvSites(),
    insights: cfg.insightsApi ? insightsClient({ apiUrl: cfg.insightsApi.url, token: cfg.insightsApi.token }) : null,
    flags: flagsDeps(cfg),
    watch: {
      store: new PostgrestWatchStore({ url: cfg.scribaseUrl, serviceRoleKey: cfg.scribaseServiceRoleKey }),
      repo,
      mailer: cfg.rallySend ? rallySend(cfg.rallySend) : noMailer,
      consoleUrl: cfg.consoleUrl,
      agentToken: cfg.watchAgentToken,
      live: cfg.watchLive,
    },
  };
  return cached;
}
