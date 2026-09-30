// Builds the service from its parts. Tests pass their own directory, flags
// reader, clock and meter fetch; server.ts passes the real ones.

import { Hono } from "hono";
import type { ServiceDeps } from "./app-deps.ts";
import { Meter, type MeterConfig } from "./capture/meter.ts";
import { CapturePipeline } from "./capture/pipeline.ts";
import { UsageCounter } from "./capture/usage.ts";
import { EventWriter } from "./capture/writer.ts";
import { Db } from "./db.ts";
import { NO_FLAGS, type FlagsReader } from "./flags/client.ts";
import { GdprHooks, type GdprHook } from "./gdpr/hooks.ts";
import { requeueStale, runDueJobs } from "./gdpr/jobs.ts";
import { runRetention } from "./jobs/retention.ts";
import { nightly, Scheduler } from "./jobs/scheduler.ts";
import { jsonLogger, type Logger, logPath } from "./log.ts";
import { replayGdprHook } from "./replay/gdpr.ts";
import { mountRoutes } from "./routes.ts";
import { surveysGdprHook } from "./surveys/gdpr.ts";
import type { SiteDirectory } from "./tenancy/site-directory.ts";

export interface ServiceOptions {
  databaseUrl: string;
  sites: SiteDirectory;
  flags?: FlagsReader;
  consoleSecret?: string | null;
  bridgeSecret?: string | null;
  meter?: MeterConfig;
  meterFetch?: typeof fetch;
  log?: Logger;
  now?: () => Date;
  hooks?: GdprHook[];
  poolSize?: number;
}

export interface Service {
  app: Hono;
  deps: ServiceDeps;
  scheduler: Scheduler;
  close(): Promise<void>;
}

export function createService(opts: ServiceOptions): Service {
  const now = opts.now ?? (() => new Date());
  const log = opts.log ?? jsonLogger();
  const db = new Db(opts.databaseUrl, opts.poolSize ?? 5);
  const writer = new EventWriter(db, log, now);
  const usage = new UsageCounter(db, (org) => writer.pendingFor(org), now);
  writer.onStored = (org, month, n) => usage.stored(org, month, n);
  const meter = new Meter(opts.meter ?? { live: false, url: null, apiKey: null }, log, opts.meterFetch);
  const pipeline = new CapturePipeline({ db, writer, usage, meter, log, now });
  const hooks = new GdprHooks([replayGdprHook, surveysGdprHook, ...(opts.hooks ?? [])]);
  const deps: ServiceDeps = {
    db,
    sites: opts.sites,
    writer,
    usage,
    meter,
    pipeline,
    flags: opts.flags ?? NO_FLAGS,
    hooks,
    log,
    now,
    consoleSecret: opts.consoleSecret ?? null,
    bridgeSecret: opts.bridgeSecret ?? null,
  };
  const app = mountRoutes(new Hono(), deps);
  app.onError((err, c) => {
    log.error("unhandled error", { path: logPath(c.req.url), error: err.message });
    return c.json({ error: "Something went wrong. Try again shortly." }, 500);
  });
  const scheduler = new Scheduler(
    [
      { name: "meter", everyMs: 60_000, run: (t) => meter.flush(t) },
      { name: "privacy", everyMs: 60_000, run: async (t) => (await requeueStale(db, t), runDueJobs(db, hooks, log, t)) },
      { name: "retention", everyMs: 20 * 3600_000, when: nightly, run: (t) => runRetention(db, opts.sites, t) },
    ],
    log,
    now,
  );
  return {
    app,
    deps,
    scheduler,
    async close() {
      scheduler.stop();
      await writer.stop();
      await meter.flush(now(), true);
      await db.close();
    },
  };
}
