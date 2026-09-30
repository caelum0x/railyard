// Survey results from `survey sent` events (the SDKs' answer event): one
// answer per submission (the latest event of a submission id), per question
// by its id ($survey_response_<id>, with the older index keys as fallback).
// Shown and dismissed counts come from `survey shown` and `survey dismissed`.

import type { Db } from "../db.ts";
import type { SurveyRow } from "./model.ts";

export interface QuestionResult {
  id: string;
  question: string;
  type: string;
  answered: number;
  /** choice or rating value -> answers */
  counts: Record<string, number>;
  /** open text and link answers, newest first (at most 50) */
  texts: string[];
}

export interface SurveyResults {
  survey_id: string;
  shown: number;
  dismissed: number;
  responses: number;
  questions: QuestionResult[];
}

type Answer = string | string[] | number | null;

export function answerOf(props: Record<string, unknown>, q: { id: string }, index: number): Answer {
  const keys = [`$survey_response_${q.id}`, index === 0 ? "$survey_response" : `$survey_response_${index}`];
  for (const k of keys) {
    const v = props[k];
    if (v === undefined || v === null || v === "") continue;
    if (Array.isArray(v)) return v.filter((x) => typeof x === "string").map((x) => String(x).slice(0, 500));
    if (typeof v === "number") return v;
    return String(v).slice(0, 2000);
  }
  return null;
}

export function tally(survey: SurveyRow, answers: Record<string, unknown>[]): QuestionResult[] {
  return survey.questions.map((q, i) => {
    const r: QuestionResult = { id: q.id, question: q.question, type: q.type, answered: 0, counts: {}, texts: [] };
    for (const props of answers) {
      const a = answerOf(props, q, i);
      if (a === null || (Array.isArray(a) && a.length === 0)) continue;
      r.answered += 1;
      if (q.type === "open" || q.type === "link") {
        if (r.texts.length < 50) r.texts.push(String(a));
      } else if (Array.isArray(a)) {
        for (const x of a) r.counts[x] = (r.counts[x] ?? 0) + 1;
      } else {
        const k = String(a);
        r.counts[k] = (r.counts[k] ?? 0) + 1;
      }
    }
    return r;
  });
}

/** Answers of a survey: the latest `survey sent` per submission, newest first. */
export async function surveyAnswers(db: Db, projectId: string, surveyId: string, limit = 100_000): Promise<Record<string, unknown>[]> {
  return db.asProject(projectId, async (c) => {
    const r = await c.query<{ properties: Record<string, unknown> }>(
      `select properties from (
         select distinct on (coalesce(properties ->> '$survey_submission_id', uuid::text)) properties, "timestamp"
         from pa.events where project_id = $1 and event = 'survey sent' and properties ->> '$survey_id' = $2
         order by coalesce(properties ->> '$survey_submission_id', uuid::text), "timestamp" desc
       ) x order by "timestamp" desc limit $3`,
      [projectId, surveyId, limit],
    );
    return r.rows.map((x) => x.properties);
  });
}

export async function surveyResults(db: Db, projectId: string, survey: SurveyRow): Promise<SurveyResults> {
  const answers = await surveyAnswers(db, projectId, survey.id);
  const seen = await db.asProject(projectId, async (c) =>
    (
      await c.query<{ event: string; n: number }>(
        `select event, count(distinct person_id)::int as n from pa.events where project_id = $1 and event in ('survey shown', 'survey dismissed') and properties ->> '$survey_id' = $2 group by event`,
        [projectId, survey.id],
      )
    ).rows,
  );
  const n = (e: string) => seen.find((x) => x.event === e)?.n ?? 0;
  return { survey_id: survey.id, shown: n("survey shown"), dismissed: n("survey dismissed"), responses: answers.length, questions: tally(survey, answers) };
}

/** Surveys whose response limit is reached (they stop showing). */
export async function atLimit(db: Db, projectId: string, surveys: SurveyRow[]): Promise<Set<string>> {
  const limited = surveys.filter((s) => s.response_limit);
  if (!limited.length) return new Set();
  const r = await db.asProject(projectId, async (c) =>
    (
      await c.query<{ id: string; n: number }>(
        `select properties ->> '$survey_id' as id, count(distinct coalesce(properties ->> '$survey_submission_id', uuid::text))::int as n
         from pa.events where project_id = $1 and event = 'survey sent' and properties ->> '$survey_id' = any($2::text[]) group by 1`,
        [projectId, limited.map((s) => s.id)],
      )
    ).rows,
  );
  return new Set(limited.filter((s) => (r.find((x) => x.id === s.id)?.n ?? 0) >= s.response_limit!).map((s) => s.id));
}
