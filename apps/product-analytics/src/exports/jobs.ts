// A3's background work on the service's in-process scheduler pattern
// (src/jobs/scheduler.ts): exports every 30 seconds and recording expiry
// every hour. Off when PA_A3_JOBS=off or under the test runner (tests drive
// the tasks by hand).

import type { ServiceDeps } from "../app-deps.ts";
import { Scheduler, type Task } from "../jobs/scheduler.ts";
import { replayStore } from "../replay/object-store.ts";
import { expireReplays } from "../replay/query.ts";
import { runExports } from "./runner.ts";

export const EXPORTS_EVERY_MS = 30_000;
export const REPLAY_EXPIRY_EVERY_MS = 3600_000;

export function a3Tasks(deps: Pick<ServiceDeps, "db" | "log">, fetchImpl?: typeof fetch): Task[] {
  return [
    { name: "a3-exports", everyMs: EXPORTS_EVERY_MS, run: (now) => runExports(deps.db, now, { log: deps.log, fetchImpl }) },
    { name: "a3-replay-expiry", everyMs: REPLAY_EXPIRY_EVERY_MS, run: (now) => expireReplays(deps.db, replayStore(), now) },
  ];
}

let started: Scheduler | null = null;

export function startA3Jobs(deps: Pick<ServiceDeps, "db" | "log" | "now">): Scheduler | null {
  if (process.env.PA_A3_JOBS === "off" || process.env.NODE_TEST_CONTEXT) return null;
  if (started) return started;
  started = new Scheduler(a3Tasks(deps), deps.log, deps.now);
  started.start(10_000);
  return started;
}
