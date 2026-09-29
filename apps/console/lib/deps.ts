// Server wiring: config, repository and payment client for route handlers
// and server components. Server-only (never imported by a "use client" file).

import { readConfig, type Config } from "./config.ts";
import { Dodo } from "./dodo.ts";
import { PostgrestRepo } from "./postgrest-repo.ts";
import type { Repo } from "./repo.ts";

export interface Deps {
  cfg: Config;
  repo: Repo;
  dodo: Dodo;
}

let cached: Deps | null = null;

export function deps(): Deps {
  if (cached) return cached;
  const cfg = readConfig();
  cached = {
    cfg,
    repo: new PostgrestRepo({ url: cfg.scribaseUrl, serviceRoleKey: cfg.scribaseServiceRoleKey }),
    dodo: new Dodo({ base: cfg.dodoBase, apiKey: cfg.dodoApiKey, allowLive: cfg.dodoAllowLive }),
  };
  return cached;
}
