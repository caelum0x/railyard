// Background work for insights, on the service's in-process scheduler
// pattern (src/jobs/scheduler.ts): rollups every minute, behavioural cohorts
// nightly, and old cached answers pruned daily. Off when PA_INSIGHTS_JOBS=off
// (tests drive the tasks by hand).

import type { ServiceDeps } from "../app-deps.ts";
import { nightly, Scheduler, type Task } from "../jobs/scheduler.ts";
import { pruneCache } from "./cache.ts";
import { recomputeAll } from "./cohorts.ts";
import { runRollups } from "./rollups.ts";

export const ROLLUP_EVERY_MS = 60_000;

export function insightsTasks(deps: Pick<ServiceDeps, "db" | "sites">): Task[] {
  return [
    { name: "insights-rollups", everyMs: ROLLUP_EVERY_MS, run: (now) => runRollups(deps.db, now, { sites: deps.sites }) },
    { name: "insights-cohorts", everyMs: 20 * 3600_000, when: nightly, run: (now) => recomputeAll(deps.db, now) },
    { name: "insights-cache-prune", everyMs: 24 * 3600_000, run: (now) => pruneCache(deps.db, now) },
  ];
}

let started: Scheduler | null = null;

/** Starts the insights scheduler once per process (its timer does not keep the process alive). */
export function startInsightsJobs(deps: Pick<ServiceDeps, "db" | "sites" | "log" | "now">): Scheduler {
  if (started) return started;
  started = new Scheduler(insightsTasks(deps), deps.log, deps.now);
  started.start();
  return started;
}
