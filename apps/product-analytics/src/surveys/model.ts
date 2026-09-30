// Survey definitions: validation of what the console sends, and the shape the
// SDKs render (their /api/surveys/ contract). Question types: open text,
// link, rating (numbers only), single and multiple choice. Targeting: a page
// address (web), a flag of the flags slot, or a cohort. Branding of the
// rendered survey is always off (whiteLabel).

import { randomUUID } from "node:crypto";
import { ApiError, text } from "../replay/shared/console-guard.ts";

export const QUESTION_TYPES = ["open", "link", "rating", "single_choice", "multiple_choice"] as const;
export type QuestionType = (typeof QUESTION_TYPES)[number];
export const MAX_QUESTIONS = 10;
export const RATING_SCALES = [3, 5, 7, 10] as const;
export const URL_MATCH = ["icontains", "exact", "regex"] as const;
export const FLAG_KEY = /^[a-z][a-z0-9_]{0,63}$/;
const HEX = /^#[0-9a-fA-F]{6}$/;

export interface Question {
  id: string;
  type: QuestionType;
  question: string;
  description: string | null;
  optional: boolean;
  buttonText: string | null;
  scale?: number;
  display?: "number";
  lowerBoundLabel?: string;
  upperBoundLabel?: string;
  choices?: string[];
  hasOpenChoice?: boolean;
  link?: string;
}

export interface Appearance {
  position: "left" | "right" | "center";
  backgroundColor: string | null;
  submitButtonColor: string | null;
  submitButtonTextColor: string | null;
  borderColor: string | null;
  thankYouMessageHeader: string | null;
  displayThankYouMessage: boolean;
}

type Obj = Record<string, unknown>;
const obj = (v: unknown): Obj => (typeof v === "object" && v !== null && !Array.isArray(v) ? (v as Obj) : {});

export function checkQuestions(raw: unknown): Question[] {
  if (!Array.isArray(raw) || raw.length < 1 || raw.length > MAX_QUESTIONS) throw new ApiError(400, `A survey has 1 to ${MAX_QUESTIONS} questions.`);
  const ids = new Set<string>();
  return raw.map((r, i) => {
    const q = obj(r);
    const n = i + 1;
    if (!QUESTION_TYPES.includes(q.type as QuestionType)) throw new ApiError(400, `Question ${n}: the type must be open, link, rating, single_choice or multiple_choice.`);
    const id = typeof q.id === "string" && /^[A-Za-z0-9-]{1,64}$/.test(q.id) ? q.id : randomUUID();
    if (ids.has(id)) throw new ApiError(400, `Question ${n}: question ids must be different.`);
    ids.add(id);
    const out: Question = {
      id,
      type: q.type as QuestionType,
      question: text(q.question, `Question ${n}`, 500)!,
      description: text(q.description, `Question ${n} description`, 1000, false),
      optional: q.optional === true,
      buttonText: text(q.buttonText, `Question ${n} button`, 50, false),
    };
    if (out.type === "rating") {
      const scale = q.scale ?? 5;
      if (!RATING_SCALES.includes(scale as (typeof RATING_SCALES)[number])) throw new ApiError(400, `Question ${n}: the rating scale must be 3, 5, 7 or 10.`);
      return { ...out, scale: scale as number, display: "number", lowerBoundLabel: text(q.lowerBoundLabel, `Question ${n} low label`, 100, false) ?? "Unlikely", upperBoundLabel: text(q.upperBoundLabel, `Question ${n} high label`, 100, false) ?? "Very likely" };
    }
    if (out.type === "single_choice" || out.type === "multiple_choice") {
      const choices = Array.isArray(q.choices) ? q.choices.map((c, j) => text(c, `Question ${n} choice ${j + 1}`, 200)!) : [];
      if (choices.length < 2 || choices.length > 20) throw new ApiError(400, `Question ${n}: offer 2 to 20 choices.`);
      if (new Set(choices).size !== choices.length) throw new ApiError(400, `Question ${n}: choices must be different.`);
      return { ...out, choices, hasOpenChoice: q.hasOpenChoice === true };
    }
    if (out.type === "link") {
      const link = text(q.link, `Question ${n} link`, 500)!;
      if (!/^https:\/\/\S+$/.test(link)) throw new ApiError(400, `Question ${n}: the link must start with https://.`);
      return { ...out, link };
    }
    return out;
  });
}

export function checkAppearance(raw: unknown): Appearance {
  const a = obj(raw);
  const color = (k: string) => {
    const v = a[k];
    if (v === undefined || v === null || v === "") return null;
    if (typeof v !== "string" || !HEX.test(v)) throw new ApiError(400, `${k} must be a color like #1d2230.`);
    return v;
  };
  const position = a.position ?? "right";
  if (position !== "left" && position !== "right" && position !== "center") throw new ApiError(400, "The position must be left, right or center.");
  return {
    position,
    backgroundColor: color("backgroundColor"),
    submitButtonColor: color("submitButtonColor"),
    submitButtonTextColor: color("submitButtonTextColor"),
    borderColor: color("borderColor"),
    thankYouMessageHeader: text(a.thankYouMessageHeader, "Thank-you message", 200, false),
    displayThankYouMessage: a.displayThankYouMessage !== false,
  };
}

export function checkUrlTarget(url: unknown, match: unknown): { url: string | null; url_match: (typeof URL_MATCH)[number] } {
  const m = (match ?? "icontains") as (typeof URL_MATCH)[number];
  if (!URL_MATCH.includes(m)) throw new ApiError(400, "The page match must be icontains, exact or regex.");
  const u = text(url, "Page address", 500, false);
  if (u && m === "regex") {
    if (u.length > 200) throw new ApiError(400, "A page pattern must be at most 200 characters.");
    try {
      new RegExp(u);
    } catch {
      throw new ApiError(400, "The page pattern is not a valid regular expression.");
    }
  }
  return { url: u, url_match: m };
}

/** The targeting flag of a cohort-targeted survey, answered per person by surveyTargetingAnswers. */
export const targetingFlagKey = (surveyId: string): string => `survey-targeting-${surveyId.replace(/-/g, "").slice(0, 16)}`;

export interface SurveyRow {
  id: string;
  name: string;
  description: string;
  type: "popover" | "api";
  questions: Question[];
  appearance: Appearance;
  url: string | null;
  url_match: (typeof URL_MATCH)[number];
  flag_key: string | null;
  cohort_id: string | null;
  status: "draft" | "running" | "stopped";
  start_date: string | null;
  end_date: string | null;
  response_limit: number | null;
  created_at: string;
  updated_at: string;
}

/** Drops unset (null) fields: the SDKs read an absent field as "use the default", but not a null one. */
const defined = <T extends object>(o: T): Partial<T> => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== null && v !== undefined)) as Partial<T>;

/** What the SDKs read from /api/surveys/. */
export function sdkSurvey(s: SurveyRow): Record<string, unknown> {
  return {
    id: s.id,
    name: s.name,
    description: s.description,
    type: s.type,
    questions: s.questions.map((q, i) => ({ ...defined(q), originalQuestionIndex: i })),
    appearance: { ...defined(s.appearance), whiteLabel: true, shuffleQuestions: false },
    conditions: s.url ? { url: s.url, urlMatchType: s.url_match } : null,
    start_date: s.start_date,
    end_date: s.end_date,
    current_iteration: null,
    current_iteration_start_date: null,
    linked_flag_key: s.flag_key,
    targeting_flag_key: s.cohort_id ? targetingFlagKey(s.id) : null,
    internal_targeting_flag_key: null,
    feature_flag_keys: [],
    enable_partial_responses: false,
    schedule: "once",
  };
}
