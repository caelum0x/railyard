// Privacy hook for recordings (handed over by A1): a person's deletion
// removes every recording of any of their ids (objects first, then rows);
// the export lists them.

import type { GdprHook } from "../gdpr/hooks.ts";
import { replayStore } from "./object-store.ts";
import { deleteReplays, replaysOfIdentities } from "./query.ts";

export const replayGdprHook: GdprHook = {
  name: "replays",
  async deletePerson(ref) {
    const list = await ref.db.asOwner((c) => replaysOfIdentities(c, ref.projectId, { distinctIds: ref.distinctIds, personIds: ref.personIds }, 100_000));
    return deleteReplays(ref.db, replayStore(), ref.projectId, list.map((r) => r.session_id));
  },
  async exportPerson(ref) {
    const list = await ref.db.asOwner((c) => replaysOfIdentities(c, ref.projectId, { distinctIds: ref.distinctIds, personIds: ref.personIds }, 100_000));
    return list.map(({ session_id, started_at, ended_at, duration_ms, urls, click_count, keypress_count }) => ({ session_id, started_at, ended_at, duration_ms, urls, click_count, keypress_count }));
  },
};
