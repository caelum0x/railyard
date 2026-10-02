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
import { CloudflareOAuth, DEFAULT_SCOPES } from "./ship/cloudflare.ts";
import { GithubApp } from "./ship/github.ts";
import { githubOidcKeys } from "./ship/http.ts";
import type { ShipDeps } from "./ship/service.ts";
import { PostgrestShipStore } from "./ship/store.ts";

export interface Deps {
  cfg: Config;
  repo: Repo;
  dodo: Dodo;
  sites: SitesStore;
  insights: InsightsClient | null;
  flags: FlagsDeps;
  watch: WatchDeps;
  /** Ship on push (PY3.1); null until its GitHub App and Cloudflare client are configured. */
  ship: ShipDeps | null;
}

function flagsDeps(cfg: Config): FlagsDeps {
  const store = new PostgrestFlagsStore({ url: cfg.scribaseUrl, serviceRoleKey: cfg.scribaseServiceRoleKey });
  const table = new TableFlagsBackend(store);
  const flagsmith = cfg.flags ? new FlagsmithBackend({ apiUrl: cfg.flags.apiUrl, adminToken: cfg.flags.adminToken }) : null;
  return { store, backend: flagsmith ?? table, backends: flagsmith ? { table, flagsmith } : { table } };
}

function shipDeps(cfg: Config, repo: Repo, sites: SitesStore): ShipDeps | null {
  if (!cfg.ship) return null;
  return {
    store: new PostgrestShipStore({ url: cfg.scribaseUrl, serviceRoleKey: cfg.scribaseServiceRoleKey }),
    repo,
    github: new GithubApp(cfg.ship.github),
    cloudflare: new CloudflareOAuth({
      clientId: cfg.ship.cloudflare.clientId,
      clientSecret: cfg.ship.cloudflare.clientSecret,
      redirectUri: `${cfg.consoleUrl}/api/v1/github/cloudflare/callback`,
      scopes: cfg.ship.cloudflare.scopes ?? DEFAULT_SCOPES,
    }),
    oidcKeys: githubOidcKeys(),
    consoleUrl: cfg.consoleUrl,
    credentialsKey: cfg.ship.credentialsKey,
    sessionSecret: cfg.sessionSecret,
    sites,
  };
}

let cached: Deps | null = null;

export function deps(): Deps {
  if (cached) return cached;
  const cfg = readConfig();
  const repo = new PostgrestRepo({ url: cfg.scribaseUrl, serviceRoleKey: cfg.scribaseServiceRoleKey });
  const sites = kvSites();
  cached = {
    cfg,
    repo,
    dodo: new Dodo({ base: cfg.dodoBase, apiKey: cfg.dodoApiKey, allowLive: cfg.dodoAllowLive }),
    sites,
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
    ship: shipDeps(cfg, repo, sites),
  };
  return cached;
}
