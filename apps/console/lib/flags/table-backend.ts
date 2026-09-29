// The table backend: flag values in Railyard's own tables (migration 0005).
// Used when Flagsmith is not configured, which is the fallback the dispatch
// names if the box 2 memory gate keeps the Flagsmith stack down.

import type { FlagsStore } from "./store.ts";
import type { Flag, FlagEnv, FlagInput, FlagsBackend } from "./types.ts";

export class TableFlagsBackend implements FlagsBackend {
  readonly name = "table" as const;
  private readonly store: FlagsStore;
  constructor(store: FlagsStore) {
    this.store = store;
  }
  async prepare() {
    return { backend_ref: null, backend_project: null, backend_org: null };
  }
  list(env: FlagEnv): Promise<Flag[]> {
    return this.store.listFlags(env.id);
  }
  set(env: FlagEnv, input: FlagInput, actor: string, now: Date): Promise<Flag> {
    return this.store.upsertFlag(env.id, input, actor, now.toISOString());
  }
}
