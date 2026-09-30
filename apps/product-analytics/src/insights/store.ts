// Saved insights and dashboards. Every read and write is scoped to one
// project (row level security plus a bound project id); a tile may only
// join a dashboard and an insight of the same project.

import type { Db } from "../db.ts";
import { isUuid } from "../db.ts";
import { computeCached, type CachedAnswer } from "./cache.ts";
import { QueryError, type InsightKind, type InsightQuery } from "./types.ts";
import { parseQuery } from "./validate.ts";

export interface SavedInsight {
  id: string;
  name: string;
  kind: InsightKind;
  query: InsightQuery;
  created_at: string;
  updated_at: string;
}

export interface DashboardSummary {
  id: string;
  name: string;
  created_at: string;
  tiles: number;
}

export interface DashboardTile {
  position: number;
  insight: SavedInsight;
  answer: CachedAnswer | null;
  error: string | null;
}

export interface Dashboard {
  id: string;
  name: string;
  created_at: string;
  tiles: DashboardTile[];
}

export const MAX_TILES = 24;

function name(v: unknown, what: string): string {
  const n = typeof v === "string" ? v.trim() : "";
  if (n.length < 1 || n.length > 200) throw new QueryError(`Give the ${what} a name of 1 to 200 characters.`);
  return n;
}

const INSIGHT_COLS = "id, name, kind, query, created_at, updated_at";

export async function listSavedInsights(db: Db, projectId: string): Promise<SavedInsight[]> {
  return db.asProject(projectId, async (c) => (await c.query<SavedInsight>(`select ${INSIGHT_COLS} from pa.saved_insights where project_id = $1 order by updated_at desc limit 500`, [projectId])).rows);
}

export async function getSavedInsight(db: Db, projectId: string, id: string): Promise<SavedInsight | null> {
  if (!isUuid(id)) return null;
  return db.asProject(projectId, async (c) => (await c.query<SavedInsight>(`select ${INSIGHT_COLS} from pa.saved_insights where project_id = $1 and id = $2`, [projectId, id])).rows[0] ?? null);
}

export async function createSavedInsight(db: Db, projectId: string, input: { name: unknown; query: unknown }): Promise<SavedInsight> {
  const n = name(input.name, "insight");
  const q = parseQuery(input.query);
  return db.asProject(projectId, async (c) =>
    (await c.query<SavedInsight>(`insert into pa.saved_insights (project_id, name, kind, query) values ($1, $2, $3, $4) returning ${INSIGHT_COLS}`, [projectId, n, q.kind, JSON.stringify(q)])).rows[0],
  );
}

export async function updateSavedInsight(db: Db, projectId: string, id: string, input: { name: unknown; query: unknown }): Promise<SavedInsight | null> {
  if (!isUuid(id)) return null;
  const n = name(input.name, "insight");
  const q = parseQuery(input.query);
  return db.asProject(projectId, async (c) =>
    (
      await c.query<SavedInsight>(
        `update pa.saved_insights set name = $3, kind = $4, query = $5, updated_at = now() where project_id = $1 and id = $2 returning ${INSIGHT_COLS}`,
        [projectId, id, n, q.kind, JSON.stringify(q)],
      )
    ).rows[0] ?? null,
  );
}

export async function deleteSavedInsight(db: Db, projectId: string, id: string): Promise<boolean> {
  if (!isUuid(id)) return false;
  return db.asProject(projectId, async (c) => ((await c.query("delete from pa.saved_insights where project_id = $1 and id = $2", [projectId, id])).rowCount ?? 0) > 0);
}

export async function listDashboards(db: Db, projectId: string): Promise<DashboardSummary[]> {
  return db.asProject(projectId, async (c) =>
    (
      await c.query<DashboardSummary>(
        `select d.id, d.name, d.created_at, (select count(*)::int from pa.dashboard_tiles t where t.project_id = d.project_id and t.dashboard_id = d.id) as tiles
           from pa.dashboards d where d.project_id = $1 order by d.name`,
        [projectId],
      )
    ).rows,
  );
}

export async function createDashboard(db: Db, projectId: string, input: { name: unknown }): Promise<DashboardSummary> {
  const n = name(input.name, "dashboard");
  try {
    return await db.asProject(projectId, async (c) => ({
      ...(await c.query<Omit<DashboardSummary, "tiles">>("insert into pa.dashboards (project_id, name) values ($1, $2) returning id, name, created_at", [projectId, n])).rows[0],
      tiles: 0,
    }));
  } catch (err) {
    if ((err as { code?: string }).code === "23505") throw new QueryError("A dashboard with that name already exists.");
    throw err;
  }
}

export async function deleteDashboard(db: Db, projectId: string, id: string): Promise<boolean> {
  if (!isUuid(id)) return false;
  return db.asProject(projectId, async (c) => ((await c.query("delete from pa.dashboards where project_id = $1 and id = $2", [projectId, id])).rowCount ?? 0) > 0);
}

export async function addTile(db: Db, projectId: string, dashboardId: string, insightId: string): Promise<void> {
  if (!isUuid(dashboardId) || !isUuid(insightId)) throw new QueryError("Pick a dashboard and an insight.");
  await db.asProject(projectId, async (c) => {
    const ok = await c.query(
      "select (select count(*) from pa.dashboards where project_id = $1 and id = $2) as d, (select count(*) from pa.saved_insights where project_id = $1 and id = $3) as i",
      [projectId, dashboardId, insightId],
    );
    const row = ok.rows[0] as { d: string; i: string };
    if (Number(row.d) !== 1) throw new QueryError("That dashboard is not in this app.");
    if (Number(row.i) !== 1) throw new QueryError("That insight is not in this app.");
    const n = await c.query<{ n: number; exists: boolean }>(
      "select count(*)::int as n, bool_or(insight_id = $3) as exists from pa.dashboard_tiles where project_id = $1 and dashboard_id = $2",
      [projectId, dashboardId, insightId],
    );
    if (n.rows[0].exists) throw new QueryError("That insight is already on the dashboard.");
    if (n.rows[0].n >= MAX_TILES) throw new QueryError(`A dashboard holds up to ${MAX_TILES} insights.`);
    await c.query(
      `insert into pa.dashboard_tiles (project_id, dashboard_id, insight_id, position)
       values ($1, $2, $3, coalesce((select max(position) + 1 from pa.dashboard_tiles where project_id = $1 and dashboard_id = $2), 0))`,
      [projectId, dashboardId, insightId],
    );
  });
}

export async function removeTile(db: Db, projectId: string, dashboardId: string, insightId: string): Promise<boolean> {
  if (!isUuid(dashboardId) || !isUuid(insightId)) return false;
  return db.asProject(projectId, async (c) => ((await c.query("delete from pa.dashboard_tiles where project_id = $1 and dashboard_id = $2 and insight_id = $3", [projectId, dashboardId, insightId])).rowCount ?? 0) > 0);
}

/** A dashboard with each tile's answer: cached answers as they are (stale ones say so), or all recomputed on refresh. */
export async function getDashboard(db: Db, projectId: string, id: string, now: Date, opts: { refresh: boolean }): Promise<Dashboard | null> {
  if (!isUuid(id)) return null;
  const found = await db.asProject(projectId, async (c) => {
    const d = (await c.query<Omit<Dashboard, "tiles">>("select id, name, created_at from pa.dashboards where project_id = $1 and id = $2", [projectId, id])).rows[0];
    if (!d) return null;
    const tiles = (
      await c.query<SavedInsight & { position: number }>(
        `select i.id, i.name, i.kind, i.query, i.created_at, i.updated_at, t.position
           from pa.dashboard_tiles t join pa.saved_insights i on i.project_id = t.project_id and i.id = t.insight_id
          where t.project_id = $1 and t.dashboard_id = $2 order by t.position, i.name`,
        [projectId, id],
      )
    ).rows;
    return { d, tiles };
  });
  if (!found) return null;
  const tiles: DashboardTile[] = [];
  for (const t of found.tiles) {
    const { position, ...insight } = t;
    try {
      const answer = await computeCached(db, projectId, parseQuery(insight.query), now, opts.refresh ? "force" : "prefer_cache");
      tiles.push({ position, insight, answer, error: null });
    } catch (err) {
      const status = (err as { status?: number }).status;
      if (status !== 400) throw err;
      tiles.push({ position, insight, answer: null, error: (err as Error).message });
    }
  }
  return { ...found.d, tiles };
}
