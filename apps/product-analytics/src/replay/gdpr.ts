// Privacy hook for recordings. Owned by A3 (handed over by A1: "then"):
// A3 deletes and lists the person's recordings here. Until recordings are
// stored there is nothing to delete or export.

import type { GdprHook } from "../gdpr/hooks.ts";

export const replayGdprHook: GdprHook = {
  name: "replays",
  deletePerson: async () => 0,
  exportPerson: async () => [],
};
