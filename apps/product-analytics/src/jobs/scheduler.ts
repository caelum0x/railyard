// In-process scheduler with an injectable clock. `tick(now)` runs every task
// that is due; tests drive it with a moved clock, the server with a timer.

import type { Logger } from "../log.ts";

export interface Task {
  name: string;
  everyMs: number;
  /** Optional gate, e.g. "only between 03:00 and 04:00 UTC". */
  when?: (now: Date) => boolean;
  run(now: Date): Promise<unknown>;
}

export class Scheduler {
  tasks: Task[];
  log: Logger;
  now: () => Date;
  last = new Map<string, number>();
  running = new Set<string>();
  timer: NodeJS.Timeout | null = null;

  constructor(tasks: Task[], log: Logger, now: () => Date = () => new Date()) {
    this.tasks = tasks;
    this.log = log;
    this.now = now;
  }

  async tick(now: Date = this.now()): Promise<string[]> {
    const ran: string[] = [];
    for (const t of this.tasks) {
      const last = this.last.get(t.name);
      if (this.running.has(t.name)) continue;
      if (last !== undefined && now.getTime() - last < t.everyMs) continue;
      if (t.when && !t.when(now)) continue;
      this.running.add(t.name);
      this.last.set(t.name, now.getTime());
      try {
        await t.run(now);
        ran.push(t.name);
      } catch (err) {
        this.log.error("scheduled task failed", { task: t.name, error: (err as Error).message });
      } finally {
        this.running.delete(t.name);
      }
    }
    return ran;
  }

  start(intervalMs = 15_000): void {
    if (this.timer) return;
    this.timer = setInterval(() => void this.tick(), intervalMs);
    this.timer.unref();
  }

  stop(): void {
    if (this.timer) clearInterval(this.timer);
    this.timer = null;
  }
}

/** Nightly window: 03:00-03:59 UTC. */
export const nightly = (now: Date) => now.getUTCHours() === 3;
