// Surveys per project (pa.surveys): create, edit, start, stop, delete. At most
// MAX_RUNNING run at once (target). A cohort must be one of the project's.

import type { Client, Db } from "../db.ts";
import { isUuid } from "../db.ts";
import { ApiError, text } from "../replay/shared/console-guard.ts";
import { checkAppearance, checkQuestions, checkUrlTarget, FLAG_KEY, sdkSurvey, type SurveyRow } from "./model.ts";
import { atLimit } from "./results.ts";

/** Running surveys per project (target, copy-source: apps/product-analytics/src/surveys/store.ts). */
export const MAX_RUNNING = 5;

const COLS = "id, name, description, type, questions, appearance, url, url_match, flag_key, cohort_id, status, start_date, end_date, response_limit, created_at, updated_at";
const iso = (v: unknown) => (v instanceof Date ? v.toISOString() : ((v as string | null) ?? null));
const row = (r: Record<string, unknown>): SurveyRow =>
  ({ ...(r as unknown as SurveyRow), start_date: iso(r.start_date), end_date: iso(r.end_date), created_at: iso(r.created_at)!, updated_at: iso(r.updated_at)! });

export interface SurveyInput {
  name: string;
  description: string;
  type: "popover" | "api";
  questions: SurveyRow["questions"];
  appearance: SurveyRow["appearance"];
  url: string | null;
  url_match: SurveyRow["url_match"];
  flag_key: string | null;
  cohort_id: string | null;
  response_limit: number | null;
}

export function checkSurvey(b: Record<string, unknown>): SurveyInput {
  const type = b.type ?? "popover";
  if (type !== "popover" && type !== "api") throw new ApiError(400, "The survey type must be popover or api.");
  const flag = b.flag_key === undefined || b.flag_key === null || b.flag_key === "" ? null : b.flag_key;
  if (flag !== null && (typeof flag !== "string" || !FLAG_KEY.test(flag))) throw new ApiError(400, "The flag key must start with a letter and use lowercase letters, digits and underscores.");
  const cohort = b.cohort_id === undefined || b.cohort_id === null || b.cohort_id === "" ? null : b.cohort_id;
  if (cohort !== null && !isUuid(cohort)) throw new ApiError(400, "Choose a cohort from the list.");
  const limit = b.response_limit === undefined || b.response_limit === null || b.response_limit === "" ? null : b.response_limit;
  if (limit !== null && (typeof limit !== "number" || !Number.isInteger(limit) || limit < 1 || limit > 1_000_000)) throw new ApiError(400, "The response limit must be a whole number.");
  return {
    name: text(b.name, "Name", 200)!,
    description: text(b.description, "Description", 1000, false) ?? "",
    type,
    questions: checkQuestions(b.questions),
    appearance: checkAppearance(b.appearance),
    ...checkUrlTarget(b.url, b.url_match),
    flag_key: flag as string | null,
    cohort_id: cohort as string | null,
    response_limit: limit as number | null,
  };
}

async function checkCohort(c: Client, projectId: string, cohortId: string | null): Promise<void> {
  if (!cohortId) return;
  const r = await c.query("select 1 from pa.cohorts where project_id = $1 and id = $2", [projectId, cohortId]);
  if (!r.rows[0]) throw new ApiError(400, "That cohort is not in this app.");
}

function conflict(err: unknown): never {
  if ((err as { code?: string }).code === "23505") throw new ApiError(409, "A survey with that name exists.");
  throw err;
}

export function listSurveys(db: Db, projectId: string): Promise<SurveyRow[]> {
  return db.asProject(projectId, async (c) => (await c.query(`select ${COLS} from pa.surveys where project_id = $1 order by created_at desc limit 200`, [projectId])).rows.map(row));
}

export function runningSurveys(db: Db, projectId: string): Promise<SurveyRow[]> {
  return db.asProject(projectId, async (c) => (await c.query(`select ${COLS} from pa.surveys where project_id = $1 and status = 'running' order by created_at`, [projectId])).rows.map(row));
}

/** Running surveys that have not reached their response limit, in the SDKs' shape. */
export async function activeSurveys(db: Db, projectId: string): Promise<Record<string, unknown>[]> {
  const running = await runningSurveys(db, projectId);
  const full = await atLimit(db, projectId, running);
  return running.filter((s) => !full.has(s.id)).map(sdkSurvey);
}

export async function getSurvey(c: Client, projectId: string, id: string): Promise<SurveyRow> {
  const r = await c.query(`select ${COLS} from pa.surveys where project_id = $1 and id = $2`, [projectId, id]);
  if (!r.rows[0]) throw new ApiError(404, "That survey does not exist.");
  return row(r.rows[0]);
}

export async function createSurvey(db: Db, projectId: string, s: SurveyInput, now: Date): Promise<SurveyRow> {
  return db
    .asProject(projectId, async (c) => {
      await checkCohort(c, projectId, s.cohort_id);
      const r = await c.query(
        `insert into pa.surveys (project_id, name, description, type, questions, appearance, url, url_match, flag_key, cohort_id, response_limit, created_at, updated_at)
         values ($1, $2, $3, $4, $5::jsonb, $6::jsonb, $7, $8, $9, $10, $11, $12, $12) returning ${COLS}`,
        [projectId, s.name, s.description, s.type, JSON.stringify(s.questions), JSON.stringify(s.appearance), s.url, s.url_match, s.flag_key, s.cohort_id, s.response_limit, now.toISOString()],
      );
      return row(r.rows[0]);
    })
    .catch(conflict);
}

/** A draft changes freely. A running or stopped survey keeps its questions (answers refer to them); its name, targeting and look may change. */
export async function updateSurvey(db: Db, projectId: string, id: string, b: Record<string, unknown>, now: Date): Promise<SurveyRow> {
  return db
    .asProject(projectId, async (c) => {
      const cur = await getSurvey(c, projectId, id);
      if (cur.status !== "draft" && b.questions !== undefined && JSON.stringify(b.questions) !== JSON.stringify(cur.questions)) {
        throw new ApiError(409, "Questions of a started survey cannot change. Make a new survey instead.");
      }
      const s = checkSurvey({ ...cur, ...b, questions: b.questions ?? cur.questions });
      await checkCohort(c, projectId, s.cohort_id);
      const r = await c.query(
        `update pa.surveys set name = $3, description = $4, type = $5, questions = $6::jsonb, appearance = $7::jsonb, url = $8, url_match = $9,
           flag_key = $10, cohort_id = $11, response_limit = $12, updated_at = $13 where project_id = $1 and id = $2 returning ${COLS}`,
        [projectId, id, s.name, s.description, s.type, JSON.stringify(s.questions), JSON.stringify(s.appearance), s.url, s.url_match, s.flag_key, s.cohort_id, s.response_limit, now.toISOString()],
      );
      return row(r.rows[0]);
    })
    .catch(conflict);
}

export async function setSurveyStatus(db: Db, projectId: string, id: string, status: "running" | "stopped", now: Date): Promise<SurveyRow> {
  return db.asProject(projectId, async (c) => {
    const cur = await getSurvey(c, projectId, id);
    if (cur.status === status) throw new ApiError(409, `The survey is already ${status}.`);
    if (status === "running") {
      const n = (await c.query<{ n: number }>("select count(*)::int as n from pa.surveys where project_id = $1 and status = 'running'", [projectId])).rows[0].n;
      if (n >= MAX_RUNNING) throw new ApiError(409, `An app can run ${MAX_RUNNING} surveys at once. Stop one first.`);
    }
    const r = await c.query(
      `update pa.surveys set status = $3, start_date = case when $3 = 'running' then $4::timestamptz else start_date end,
         end_date = case when $3 = 'stopped' then $4::timestamptz else null end, updated_at = $4
       where project_id = $1 and id = $2 returning ${COLS}`,
      [projectId, id, status, now.toISOString()],
    );
    return row(r.rows[0]);
  });
}

export async function deleteSurvey(db: Db, projectId: string, id: string): Promise<void> {
  await db.asProject(projectId, async (c) => {
    const cur = await getSurvey(c, projectId, id);
    if (cur.status === "running") throw new ApiError(409, "Stop the survey before deleting it.");
    await c.query("delete from pa.surveys where project_id = $1 and id = $2", [projectId, id]);
  });
}
