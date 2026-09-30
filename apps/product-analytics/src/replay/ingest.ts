// Recording ingest: one gzip NDJSON chunk per session and request, written to
// the object store first, then indexed in one transaction (pa.replays upsert
// plus a pa.replay_chunks row). A session stops being stored once it is
// longer than MAX_RECORDING_MS; recordings expire after the plan's replay days.

import { createHash, randomUUID } from "node:crypto";
import { gzipSync } from "fflate";
import type { Db } from "../db.ts";
import type { Logger } from "../log.ts";
import { personIdFor } from "../persons/ids.ts";
import type { Site } from "../tenancy/site-directory.ts";
import { MAX_INDEX_URLS, MAX_RECORDING_MS, replayRetentionDays } from "./limits.ts";
import type { ObjectStore } from "./object-store.ts";
import { statsOf, type SnapshotBatch } from "./snapshot.ts";

const DAY_MS = 86_400_000;
/** Recording events stamped further than this from the server clock are dropped. */
export const MAX_CLOCK_SKEW_MS = 2 * DAY_MS;

export interface ChunkLine {
  window_id: string | null;
  data: unknown;
}

export function chunkKey(projectId: string, sessionId: string): string {
  const s = createHash("sha256").update(sessionId).digest("hex").slice(0, 32);
  return `replays/${projectId}/${s}/${randomUUID()}.ndjson.gz`;
}

export function encodeChunk(batches: SnapshotBatch[]): { raw: number; gz: Uint8Array; events: number } {
  const lines: string[] = [];
  for (const b of batches) for (const e of b.events) lines.push(JSON.stringify({ window_id: b.windowId, data: e } satisfies ChunkLine));
  const raw = new TextEncoder().encode(`${lines.join("\n")}\n`);
  return { raw: raw.length, gz: gzipSync(raw, { level: 6 }), events: lines.length };
}

export interface IngestResult {
  stored: number;
  dropped: number;
}

export class ReplayIngest {
  db: Db;
  store: ObjectStore;
  log: Logger;
  now: () => Date;
  constructor(db: Db, store: ObjectStore, log: Logger, now: () => Date) {
    this.db = db;
    this.store = store;
    this.log = log;
    this.now = now;
  }

  /** Stores the batches of one project (already masked); returns chunks stored and dropped. */
  async ingest(site: Site, batches: SnapshotBatch[]): Promise<IngestResult> {
    const now = this.now().getTime();
    const bySession = new Map<string, SnapshotBatch[]>();
    for (const b of batches) {
      const events = b.events.filter((e) => Math.abs(e.timestamp - now) <= MAX_CLOCK_SKEW_MS);
      if (!b.distinctId || events.length === 0) continue;
      bySession.set(b.sessionId, [...(bySession.get(b.sessionId) ?? []), { ...b, events }]);
    }
    let stored = 0;
    let dropped = 0;
    for (const [sessionId, group] of bySession) {
      if (await this.storeSession(site, sessionId, group)) stored += 1;
      else dropped += 1;
    }
    return { stored, dropped };
  }

  async storeSession(site: Site, sessionId: string, group: SnapshotBatch[]): Promise<boolean> {
    const pid = site.project_id;
    const stats = statsOf(group.flatMap((b) => b.events));
    const existing = await this.db.asProject(pid, async (c) => (await c.query<{ started_at: Date }>("select started_at from pa.replays where project_id = $1 and session_id = $2", [pid, sessionId])).rows[0]);
    const start = Math.min(existing ? existing.started_at.getTime() : Infinity, stats.firstTs);
    if (stats.lastTs - start > MAX_RECORDING_MS) {
      if (existing) await this.db.asProject(pid, (c) => c.query("update pa.replays set dropped_chunks = dropped_chunks + 1 where project_id = $1 and session_id = $2", [pid, sessionId]));
      return false;
    }
    const chunk = encodeChunk(group);
    const key = chunkKey(pid, sessionId);
    await this.store.put(key, chunk.gz, "application/x-ndjson");
    const distinctId = group[0].distinctId!;
    const expires = new Date(stats.firstTs + replayRetentionDays(site) * DAY_MS).toISOString();
    try {
      await this.db.asProject(pid, async (c) => {
        const person = await c.query<{ person_id: string }>("select person_id from pa.person_distinct_ids where project_id = $1 and distinct_id = $2", [pid, distinctId]);
        const personId = person.rows[0]?.person_id ?? personIdFor(pid, distinctId);
        const r = await c.query<{ chunk_count: number }>(
          `insert into pa.replays (project_id, session_id, distinct_id, person_id, started_at, ended_at, click_count, keypress_count, event_count, urls, size_bytes, raw_bytes, chunk_count, expires_at)
           values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10::text[], $11, $12, 1, $13)
           on conflict (project_id, session_id) do update set
             started_at = least(pa.replays.started_at, excluded.started_at),
             ended_at = greatest(pa.replays.ended_at, excluded.ended_at),
             click_count = pa.replays.click_count + excluded.click_count,
             keypress_count = pa.replays.keypress_count + excluded.keypress_count,
             event_count = pa.replays.event_count + excluded.event_count,
             urls = (select coalesce(array_agg(u order by o), '{}') from (
                       select u, min(o) as o from unnest(pa.replays.urls || excluded.urls) with ordinality t(u, o) group by u order by min(o) limit ${MAX_INDEX_URLS}) s),
             size_bytes = pa.replays.size_bytes + excluded.size_bytes,
             raw_bytes = pa.replays.raw_bytes + excluded.raw_bytes,
             chunk_count = pa.replays.chunk_count + 1,
             expires_at = least(pa.replays.expires_at, excluded.expires_at)
           returning chunk_count`,
          [pid, sessionId, distinctId, personId, new Date(stats.firstTs).toISOString(), new Date(stats.lastTs).toISOString(), stats.clicks, stats.keypresses, chunk.events, stats.urls.slice(0, MAX_INDEX_URLS), chunk.gz.length, chunk.raw, expires],
        );
        await c.query(
          `insert into pa.replay_chunks (project_id, session_id, seq, object_key, window_ids, first_ts, last_ts, events, bytes, raw_bytes)
           values ($1, $2, $3, $4, $5::text[], $6, $7, $8, $9, $10)`,
          [pid, sessionId, r.rows[0].chunk_count, key, [...new Set(group.map((b) => b.windowId).filter((w): w is string => !!w))], new Date(stats.firstTs).toISOString(), new Date(stats.lastTs).toISOString(), chunk.events, chunk.gz.length, chunk.raw],
        );
      });
    } catch (err) {
      await this.store.delete(key).catch(() => undefined);
      throw err;
    }
    return true;
  }
}
