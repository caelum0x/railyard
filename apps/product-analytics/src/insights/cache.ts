// The result cache. Key: a hash of the normalized query and its resolved
// range (relative ranges roll over once a day, hour ranges once an hour).
// Each answer is stored with the project's data version (stored events plus
// a counter bumped when cohorts, actions or people change). Modes:
//   prefer_cache  any cached answer, stale or not (dashboards); compute if none;
//   fresh         a cached answer only if its data version is current;
//   force         always compute (the Refresh action).

import { createHash } from "node:crypto";
import type { Db } from "../db.ts";
import { runInsight, type InsightResult } from "./runner.ts";
import { resolveRange } from "./sql.ts";
import type { InsightQuery } from "./types.ts";

export type CacheMode = "prefer_cache" | "fresh" | "force";

export interface CachedAnswer {
  result: InsightResult;
  computed_at: string;
  /** True when served from the cache. */
  cached: boolean;
  /** True when newer data exists than this answer saw. */
  stale: boolean;
  took_ms: number;
}

export const CACHE_KEEP_DAYS = 7;

function canonical(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(canonical);
  if (v && typeof v === "object") {
    return Object.fromEntries(
      Object.keys(v as Record<string, unknown>)
        .filter((k) => (v as Record<string, unknown>)[k] !== undefined)
        .sort()
        .map((k) => [k, canonical((v as Record<string, unknown>)[k])]),
    );
  }
  return v;
}

export function cacheKey(q: InsightQuery, now: Date): string {
  const r = resolveRange(q.date_range, now);
  const hour = 3600_000;
  const from = new Date(Math.floor(r.from.getTime() / hour) * hour).toISOString();
  const to = q.date_range.date_to ? r.to.toISOString() : `now:${now.toISOString().slice(0, 10)}`;
  return createHash("sha256").update(JSON.stringify([canonical(q), from, to])).digest("hex");
}

export async function dataVersion(db: Db, projectId: string): Promise<string> {
  return db.asProject(projectId, async (c) => {
    const r = await c.query<{ v: string }>(
      `select (select coalesce(sum(stored), 0) from pa.usage_monthly where project_id = $1)::text || ':' ||
              coalesce((select version from pa.insight_versions where project_id = $1), 0)::text as v`,
      [projectId],
    );
    return r.rows[0].v;
  });
}

interface Row {
  result: InsightResult;
  data_version: string;
  computed_at: Date;
  took_ms: number;
}

export async function computeCached(db: Db, projectId: string, q: InsightQuery, now: Date, mode: CacheMode): Promise<CachedAnswer> {
  const key = cacheKey(q, now);
  const version = await dataVersion(db, projectId);
  if (mode !== "force") {
    const hit = await db.asProject(projectId, async (c) =>
      (await c.query<Row>("select result, data_version, computed_at, took_ms from pa.insight_cache where project_id = $1 and cache_key = $2", [projectId, key])).rows[0] ?? null,
    );
    if (hit && (mode === "prefer_cache" || hit.data_version === version)) {
      return { result: hit.result, computed_at: new Date(hit.computed_at).toISOString(), cached: true, stale: hit.data_version !== version, took_ms: hit.took_ms };
    }
  }
  const started = performance.now();
  const result = await runInsight(db, projectId, q, now);
  const took = Math.round(performance.now() - started);
  const computedAt = new Date();
  await db.asProject(projectId, (c) =>
    c.query(
      `insert into pa.insight_cache (project_id, cache_key, result, data_version, computed_at, took_ms) values ($1, $2, $3, $4, $5, $6)
       on conflict (project_id, cache_key) do update set result = excluded.result, data_version = excluded.data_version, computed_at = excluded.computed_at, took_ms = excluded.took_ms`,
      [projectId, key, JSON.stringify(result), version, computedAt.toISOString(), took],
    ),
  );
  return { result, computed_at: computedAt.toISOString(), cached: false, stale: false, took_ms: took };
}

/** Drops cached answers older than CACHE_KEEP_DAYS (a job; runs as the owner). */
export async function pruneCache(db: Db, now: Date): Promise<number> {
  const r = await db.asOwner((c) => c.query("delete from pa.insight_cache where computed_at < $1", [new Date(now.getTime() - CACHE_KEEP_DAYS * 86_400_000).toISOString()]));
  return r.rowCount ?? 0;
}
