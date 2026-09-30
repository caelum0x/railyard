// Finding recordings: by person (any of their ids, through merges), by an
// event (its session, with the offset of the event), by a list of people
// (for example a funnel's drop-offs), or the latest ones. Reading a
// recording streams its chunks in order. Deleting removes the objects first,
// then the index rows, so a failed delete is retried rather than orphaned.

import { gunzipSync } from "node:zlib";
import type { Client, Db } from "../db.ts";
import { isUuid } from "../db.ts";
import type { ObjectStore } from "./object-store.ts";
import { ApiError } from "./shared/console-guard.ts";

export interface ReplaySummary {
  session_id: string;
  distinct_id: string;
  person_id: string;
  started_at: string;
  ended_at: string;
  duration_ms: number;
  click_count: number;
  keypress_count: number;
  event_count: number;
  urls: string[];
  size_bytes: number;
  chunk_count: number;
  expires_at: string;
}

const COLS = `session_id, distinct_id, person_id, started_at, ended_at, click_count, keypress_count, event_count, urls, size_bytes::float8 as size_bytes, chunk_count, expires_at`;

function summary(r: Record<string, unknown>): ReplaySummary {
  const s = (r.started_at as Date).getTime();
  const e = (r.ended_at as Date).getTime();
  return {
    session_id: String(r.session_id),
    distinct_id: String(r.distinct_id),
    person_id: String(r.person_id),
    started_at: new Date(s).toISOString(),
    ended_at: new Date(e).toISOString(),
    duration_ms: e - s,
    click_count: Number(r.click_count),
    keypress_count: Number(r.keypress_count),
    event_count: Number(r.event_count),
    urls: (r.urls as string[]) ?? [],
    size_bytes: Number(r.size_bytes),
    chunk_count: Number(r.chunk_count),
    expires_at: (r.expires_at as Date).toISOString(),
  };
}

export const MAX_LIST = 100;

/** Every person id and distinct id of the person behind `distinctId` (merged people included). */
export async function identitiesOf(c: Client, projectId: string, distinctId: string): Promise<{ distinctIds: string[]; personIds: string[] }> {
  const r = await c.query<{ person_id: string }>("select person_id from pa.person_distinct_ids where project_id = $1 and distinct_id = $2", [projectId, distinctId]);
  if (!r.rows[0]) return { distinctIds: [distinctId], personIds: [] };
  return identitiesOfPeople(c, projectId, [r.rows[0].person_id]);
}

export async function identitiesOfPeople(c: Client, projectId: string, personIds: string[]): Promise<{ distinctIds: string[]; personIds: string[] }> {
  const ids = personIds.filter(isUuid);
  const people = await c.query<{ id: string }>(
    `select distinct x.id from (
       select unnest($2::uuid[]) as id
       union select o.new_person_id from pa.person_overrides o where o.project_id = $1 and o.old_person_id = any($2::uuid[])
       union select o.old_person_id from pa.person_overrides o where o.project_id = $1 and o.new_person_id = any($2::uuid[])
     ) x`,
    [projectId, ids],
  );
  const all = people.rows.map((r) => r.id);
  const d = await c.query<{ distinct_id: string }>("select distinct_id from pa.person_distinct_ids where project_id = $1 and person_id = any($2::uuid[])", [projectId, all]);
  return { distinctIds: d.rows.map((r) => r.distinct_id), personIds: all };
}

export async function replaysOfIdentities(c: Client, projectId: string, who: { distinctIds: string[]; personIds: string[] }, limit = MAX_LIST): Promise<ReplaySummary[]> {
  const r = await c.query(
    `select ${COLS} from pa.replays where project_id = $1 and (distinct_id = any($2::text[]) or person_id = any($3::uuid[])) order by started_at desc limit $4`,
    [projectId, who.distinctIds, who.personIds, limit],
  );
  return r.rows.map(summary);
}

export async function latestReplays(c: Client, projectId: string, before: Date | null, limit = MAX_LIST): Promise<ReplaySummary[]> {
  const r = await c.query(`select ${COLS} from pa.replays where project_id = $1 and ($2::timestamptz is null or started_at < $2) order by started_at desc limit $3`, [projectId, before?.toISOString() ?? null, limit]);
  return r.rows.map(summary);
}

export async function replayBySession(c: Client, projectId: string, sessionId: string): Promise<ReplaySummary | null> {
  const r = await c.query(`select ${COLS} from pa.replays where project_id = $1 and session_id = $2`, [projectId, sessionId]);
  return r.rows[0] ? summary(r.rows[0]) : null;
}

/** The recording holding an event, and where in it the event happened. */
export async function replayForEvent(c: Client, projectId: string, eventUuid: string): Promise<{ replay: ReplaySummary; offset_ms: number } | null> {
  if (!isUuid(eventUuid)) throw new ApiError(400, "The event id must be a uuid.");
  const e = await c.query<{ session_id: string | null; timestamp: Date; props_session: string | null }>(
    `select session_id, "timestamp", properties ->> '$session_id' as props_session from pa.events where project_id = $1 and uuid = $2 limit 1`,
    [projectId, eventUuid],
  );
  const row = e.rows[0];
  if (!row) throw new ApiError(404, "That event does not exist.");
  const session = row.session_id ?? row.props_session;
  if (!session) return null;
  const replay = await replayBySession(c, projectId, session);
  if (!replay) return null;
  return { replay, offset_ms: Math.max(0, row.timestamp.getTime() - Date.parse(replay.started_at)) };
}

export async function chunkKeys(c: Client, projectId: string, sessionId: string): Promise<string[]> {
  const r = await c.query<{ object_key: string }>("select object_key from pa.replay_chunks where project_id = $1 and session_id = $2 order by seq", [projectId, sessionId]);
  return r.rows.map((x) => x.object_key);
}

/** The recording's lines (one JSON object per line: {window_id, data}), chunk by chunk, in order. */
export async function* replayLines(db: Db, store: ObjectStore, projectId: string, sessionId: string): AsyncGenerator<string> {
  const keys = await db.asProject(projectId, (c) => chunkKeys(c, projectId, sessionId));
  for (const k of keys) {
    const gz = await store.get(k);
    if (!gz) continue;
    yield gunzipSync(gz).toString("utf8");
  }
}

/** Deletes recordings (objects, then rows). Returns the recordings removed. */
export async function deleteReplays(db: Db, store: ObjectStore | null, projectId: string, sessionIds: string[]): Promise<number> {
  if (sessionIds.length === 0) return 0;
  const keys = await db.asOwner(async (c) => (await c.query<{ object_key: string }>("select object_key from pa.replay_chunks where project_id = $1 and session_id = any($2::text[])", [projectId, sessionIds])).rows.map((r) => r.object_key));
  if (keys.length && !store) throw new Error("recordings exist but no object store is configured");
  for (const k of keys) await store!.delete(k);
  return db.asOwner(async (c) => (await c.query("delete from pa.replays where project_id = $1 and session_id = any($2::text[])", [projectId, sessionIds])).rowCount ?? 0);
}

/** Recordings past their expiry, in batches. Returns the number removed. */
export async function expireReplays(db: Db, store: ObjectStore | null, now: Date, batch = 200): Promise<number> {
  let total = 0;
  for (;;) {
    const due = await db.asOwner(async (c) => (await c.query<{ project_id: string; session_id: string }>("select project_id, session_id from pa.replays where expires_at <= $1 order by expires_at limit $2", [now.toISOString(), batch])).rows);
    if (due.length === 0) return total;
    const byProject = new Map<string, string[]>();
    for (const d of due) byProject.set(d.project_id, [...(byProject.get(d.project_id) ?? []), d.session_id]);
    for (const [p, sessions] of byProject) total += await deleteReplays(db, store, p, sessions);
    if (due.length < batch) return total;
  }
}
