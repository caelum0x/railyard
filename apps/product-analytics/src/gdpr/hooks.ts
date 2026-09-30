// Privacy hooks: other parts of the service (replays and survey answers, A3)
// register what they hold about a person, so one deletion or export covers
// everything. A hook must only touch rows of ctx.projectId.

import type { Db } from "../db.ts";

export interface PersonRef {
  db: Db;
  projectId: string;
  /** The person's current id and every id merged into it (including personless ids). */
  personIds: string[];
  distinctIds: string[];
  now: Date;
}

export interface GdprHook {
  /** Key in the export file and in the job result, e.g. "replays". */
  name: string;
  /** Deletes what this part holds about the person; returns the rows removed. */
  deletePerson(ref: PersonRef): Promise<number>;
  /** What this part holds about the person, for the export file. */
  exportPerson(ref: PersonRef): Promise<unknown[]>;
}

export class GdprHooks {
  hooks: GdprHook[] = [];
  constructor(hooks: GdprHook[] = []) {
    for (const h of hooks) this.register(h);
  }
  register(h: GdprHook): void {
    if (this.hooks.some((x) => x.name === h.name)) throw new Error(`a privacy hook named ${h.name} is already registered`);
    this.hooks = [...this.hooks, h];
  }
}
