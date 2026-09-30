// Entry point: node src/server.ts (Node 22.18+ runs the TypeScript directly).

import { serve } from "@hono/node-server";
import { createService } from "./app.ts";
import { readConfig } from "./config.ts";
import { HttpFlagsReader } from "./flags/client.ts";
import { ensurePartitions } from "./jobs/retention.ts";
import { jsonLogger } from "./log.ts";
import { CachedSiteDirectory, PostgrestSiteDirectory } from "./tenancy/site-directory.ts";

const cfg = readConfig();
const log = jsonLogger();
if (!cfg.directoryUrl || !cfg.directoryKey) throw new Error("SITE_DIRECTORY_URL and SITE_DIRECTORY_SERVICE_KEY are required");
const sites = new CachedSiteDirectory(new PostgrestSiteDirectory({ url: cfg.directoryUrl, serviceKey: cfg.directoryKey }));
const svc = createService({
  databaseUrl: cfg.databaseUrl,
  sites,
  flags: cfg.flagsUrl ? new HttpFlagsReader({ url: cfg.flagsUrl }) : undefined,
  consoleSecret: cfg.consoleSecret,
  bridgeSecret: cfg.bridgeSecret,
  meter: { live: cfg.live, url: cfg.ragoUrl, apiKey: cfg.ragoApiKey },
  log,
});
await ensurePartitions(svc.deps.db, new Date());
svc.deps.writer.start();
svc.scheduler.start();
const server = serve({ fetch: svc.app.fetch, port: cfg.port, hostname: "0.0.0.0" }, (info) => log.info("product analytics listening", { port: info.port, metering: cfg.live }));

const shutdown = async (signal: string) => {
  log.info("shutting down", { signal });
  server.close();
  await svc.close();
  process.exit(0);
};
process.on("SIGTERM", () => void shutdown("SIGTERM"));
process.on("SIGINT", () => void shutdown("SIGINT"));
