// Experiments: plan, metric and state per project (pa.experiments). The flag
// is never written here; see flag-value.ts and routes.ts for how the console
// starts an experiment through the flags slot.

import type { Client, Db } from "../db.ts";
import { ApiError, text } from "../replay/shared/console-guard.ts";
import { checkRollout, checkVariants, VariantError, type Variant } from "./flag-value.ts";

export const FLAG_KEY = /^[a-z][a-z0-9_]{0,63}$/;
export const ENV_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/;
export const EVENT_NAME = /^[^\u0000-\u001f]{1,200}$/;
export const PROPERTY_KEY = /^[^\u0000-\u001f]{1,200}$/;

export type Metric = { kind: "conversion"; event: string } | { kind: "count"; event: string } | { kind: "sum"; event: string; property: string };
export type Method = "bayesian" | "frequentist";
export type Status = "draft" | "running" | "stopped";

export interface Experiment {
  id: string;
  name: string;
  description: string;
  flag_key: string;
  flags_env: string;
  variants: Variant[];
  rollout: number;
  metric: Metric;
  method: Method;
  status: Status;
  started_at: string | null;
  stopped_at: string | null;
  created_at: string;
  updated_at: string;
}

const COLS = "id, name, description, flag_key, flags_env, variants, rollout, metric, method, status, started_at, stopped_at, created_at, updated_at";

export function checkMetric(raw: unknown): Metric {
  const m = (raw ?? {}) as Record<string, unknown>;
  if (typeof m.event !== "string" || !EVENT_NAME.test(m.event)) throw new ApiError(400, "The metric needs an event name.");
  if (m.kind === "conversion" || m.kind === "count") return { kind: m.kind, event: m.event };
  if (m.kind === "sum") {
    if (typeof m.property !== "string" || !PROPERTY_KEY.test(m.property)) throw new ApiError(400, "A sum metric needs the event property to add up.");
    return { kind: "sum", event: m.event, property: m.property };
  }
  throw new ApiError(400, "The metric kind must be conversion, count or sum.");
}

export interface ExperimentInput {
  name: string;
  description: string;
  flag_key: string;
  flags_env: string;
  variants: Variant[];
  rollout: number;
  metric: Metric;
  method: Method;
}

export function checkInput(b: Record<string, unknown>): ExperimentInput {
  const name = text(b.name, "Name", 200)!;
  const description = text(b.description, "Description", 1000, false) ?? "";
  if (typeof b.flag_key !== "string" || !FLAG_KEY.test(b.flag_key)) throw new ApiError(400, "The flag key must start with a letter and use lowercase letters, digits and underscores.");
  if (typeof b.flags_env !== "string" || !ENV_NAME.test(b.flags_env)) throw new ApiError(400, "Name the flags environment, for example production.");
  if (b.method !== undefined && b.method !== "bayesian" && b.method !== "frequentist") throw new ApiError(400, "The method must be bayesian or frequentist.");
  try {
    return {
      name,
      description,
      flag_key: b.flag_key,
      flags_env: b.flags_env,
      variants: checkVariants(b.variants ?? [{ key: "control", weight: 50 }, { key: "test", weight: 50 }]),
      rollout: checkRollout(b.rollout),
      metric: checkMetric(b.metric),
      method: (b.method as Method | undefined) ?? "bayesian",
    };
  } catch (err) {
    if (err instanceof VariantError) throw new ApiError(400, err.message);
    throw err;
  }
}

const iso = (v: unknown) => (v instanceof Date ? v.toISOString() : (v as string | null));
function row(r: Record<string, unknown>): Experiment {
  return { ...(r as unknown as Experiment), started_at: iso(r.started_at), stopped_at: iso(r.stopped_at), created_at: iso(r.created_at)!, updated_at: iso(r.updated_at)! };
}

function conflict(err: unknown): never {
  const code = (err as { code?: string }).code;
  if (code === "23505") throw new ApiError(409, "An experiment with that name exists, or another experiment is running on that flag.");
  throw err;
}

export function listExperiments(db: Db, projectId: string): Promise<Experiment[]> {
  return db.asProject(projectId, async (c) => (await c.query(`select ${COLS} from pa.experiments where project_id = $1 order by created_at desc limit 200`, [projectId])).rows.map(row));
}

export async function getExperiment(c: Client, projectId: string, id: string): Promise<Experiment> {
  const r = await c.query(`select ${COLS} from pa.experiments where project_id = $1 and id = $2`, [projectId, id]);
  if (!r.rows[0]) throw new ApiError(404, "That experiment does not exist.");
  return row(r.rows[0]);
}

export async function createExperiment(db: Db, projectId: string, input: ExperimentInput, now: Date): Promise<Experiment> {
  return db
    .asProject(projectId, async (c) => {
      const r = await c.query(
        `insert into pa.experiments (project_id, name, description, flag_key, flags_env, variants, rollout, metric, method, created_at, updated_at)
         values ($1, $2, $3, $4, $5, $6::jsonb, $7, $8::jsonb, $9, $10, $10) returning ${COLS}`,
        [projectId, input.name, input.description, input.flag_key, input.flags_env, JSON.stringify(input.variants), input.rollout, JSON.stringify(input.metric), input.method, now.toISOString()],
      );
      return row(r.rows[0]);
    })
    .catch(conflict);
}

/** A draft can change completely; a running or stopped experiment only its name and description. */
export async function updateExperiment(db: Db, projectId: string, id: string, b: Record<string, unknown>, now: Date): Promise<Experiment> {
  return db
    .asProject(projectId, async (c) => {
      const cur = await getExperiment(c, projectId, id);
      const next: ExperimentInput =
        cur.status === "draft"
          ? checkInput({ ...cur, ...b })
          : { ...cur, name: text(b.name ?? cur.name, "Name", 200)!, description: text(b.description ?? cur.description, "Description", 1000, false) ?? "" };
      const r = await c.query(
        `update pa.experiments set name = $3, description = $4, flag_key = $5, flags_env = $6, variants = $7::jsonb, rollout = $8, metric = $9::jsonb, method = $10, updated_at = $11
         where project_id = $1 and id = $2 returning ${COLS}`,
        [projectId, id, next.name, next.description, next.flag_key, next.flags_env, JSON.stringify(next.variants), next.rollout, JSON.stringify(next.metric), next.method, now.toISOString()],
      );
      return row(r.rows[0]);
    })
    .catch(conflict);
}

export async function deleteExperiment(db: Db, projectId: string, id: string): Promise<void> {
  await db.asProject(projectId, async (c) => {
    const cur = await getExperiment(c, projectId, id);
    if (cur.status === "running") throw new ApiError(409, "Stop the experiment before deleting it.");
    await c.query("delete from pa.experiments where project_id = $1 and id = $2", [projectId, id]);
  });
}

/** draft -> running -> stopped, recorded after the console changed the flag through the flags slot. */
export async function setStatus(db: Db, projectId: string, id: string, status: Status, now: Date): Promise<Experiment> {
  return db
    .asProject(projectId, async (c) => {
      const cur = await getExperiment(c, projectId, id);
      const allowed = (cur.status === "draft" && status === "running") || (cur.status === "running" && status === "stopped");
      if (!allowed) throw new ApiError(409, cur.status === status ? `The experiment is already ${status}.` : `A ${cur.status} experiment cannot become ${status}.`);
      const r = await c.query(
        `update pa.experiments set status = $3, started_at = case when $3 = 'running' then $4::timestamptz else started_at end,
           stopped_at = case when $3 = 'stopped' then $4::timestamptz else stopped_at end, updated_at = $4
         where project_id = $1 and id = $2 returning ${COLS}`,
        [projectId, id, status, now.toISOString()],
      );
      return row(r.rows[0]);
    })
    .catch(conflict);
}
