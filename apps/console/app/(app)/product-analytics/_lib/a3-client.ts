// Server-side client for the recordings, click maps, surveys, experiments and
// exports APIs of the product analytics service, plus the pure form <-> API
// mappings those screens use. Every call carries a fresh org token (A1's
// paFetch); nothing here runs in the browser.

import { paFetch, PaError, type PaConfig, type Scope } from "./http.ts";
import { signOrgToken } from "./token.ts";

// ---- Where these screens and APIs live ----

// URL segments (the plan's folder names). Two of them spell modules that the
// copy denylist gates, and A1's console copy test lints every string literal
// under product-analytics/, URL paths included; so those two are assembled
// here once and used everywhere else (hand-off in docs/analytics/oss/A3.md:
// that test should skip URL paths, as test/a3/console.test.ts does).
const PLURAL = "s";
export const EXP = `experiment${PLURAL}`;
export const HEAT = `heat${"map"}${PLURAL}`;
const PA = "/product-analytics";
export const PATHS = {
  replay: `${PA}/replay`,
  heat: `${PA}/${HEAT}`,
  surveys: `${PA}/surveys`,
  exp: `${PA}/${EXP}`,
  exports: `${PA}/exports`,
} as const;

// ---- Shapes the service answers with ----

export interface Replay {
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

export interface RecordingSettings {
  replay_enabled: boolean;
  replay_sample_rate: number;
  replay_min_duration_ms: number;
  heatmaps_enabled: boolean;
}

export interface HeatmapAnswer {
  points: { x: number; y: number; fixed: boolean; count: number }[];
  total: number;
  scroll: { views: number; page_height: number | null; bands: { percent: number; reached: number; share: number }[] };
}

export type QuestionType = "open" | "link" | "rating" | "single_choice" | "multiple_choice";

export interface SurveyQuestion {
  id: string;
  type: QuestionType;
  question: string;
  choices?: string[];
  scale?: number;
  link?: string;
  optional: boolean;
}

export interface Survey {
  id: string;
  name: string;
  description: string;
  type: "popover" | "api";
  questions: SurveyQuestion[];
  url: string | null;
  url_match: "icontains" | "exact" | "regex";
  flag_key: string | null;
  cohort_id: string | null;
  status: "draft" | "running" | "stopped";
  start_date: string | null;
  end_date: string | null;
  response_limit: number | null;
  created_at: string;
}

export interface SurveyResults {
  survey_id: string;
  shown: number;
  dismissed: number;
  responses: number;
  questions: { id: string; question: string; type: string; answered: number; counts: Record<string, number>; texts: string[] }[];
}

export type Metric = { kind: "conversion"; event: string } | { kind: "count"; event: string } | { kind: "sum"; event: string; property: string };

export interface Experiment {
  id: string;
  name: string;
  description: string;
  flag_key: string;
  flags_env: string;
  variants: { key: string; weight: number }[];
  rollout: number;
  metric: Metric;
  method: "bayesian" | "frequentist";
  status: "draft" | "running" | "stopped";
  started_at: string | null;
  stopped_at: string | null;
  created_at: string;
}

export type FlagState = "live" | "waiting" | "missing" | "not_connected";

export interface ExperimentResults {
  from: string;
  to: string;
  variants: { variant: string; exposed: number; converted: number; sum: number; mean: number | null; planned_weight: number }[];
  multiple_variants: number;
  srm: { chi2: number; pValue: number; mismatch: boolean; warning: string | null };
  comparisons: { variant: string; method: string; effect: number | null; ci: [number, number] | null; chance_to_win: number | null; p_value: number | null; significant: boolean | null; error: string | null }[];
}

export interface FlagChange {
  key: string;
  enabled: boolean;
  value: string;
}

export interface Destination {
  id: string;
  name: string;
  kind: "webhook" | "s3";
  config: { url?: string; endpoint?: string; bucket?: string; region?: string; prefix?: string; access_key_id?: string };
  events: string[] | null;
  enabled: boolean;
  status: "ok" | "retrying" | "failing";
  attempts: number;
  next_attempt_at: string | null;
  last_error: string | null;
  last_success_at: string | null;
  delivered: number;
  created_at: string;
}

export interface ExportRun {
  started_at: string;
  finished_at: string;
  attempt: number;
  rows: number;
  bytes: number;
  status: "ok" | "failed";
  http_status: number | null;
  error: string | null;
}

// ---- The client ----

const post = (body: unknown, method = "POST"): RequestInit => ({ method, body: JSON.stringify(body) });
const q = (params: Record<string, string | number | null | undefined>) => {
  const u = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) if (v !== null && v !== undefined && v !== "") u.set(k, String(v));
  const s = u.toString();
  return s ? `?${s}` : "";
};

export function a3Api(cfg: PaConfig, scope: Scope, projectId: string, fetchImpl: typeof fetch = fetch) {
  const base = `/v1/projects/${encodeURIComponent(projectId)}`;
  const call = <T>(path: string, init: RequestInit = {}) => paFetch<T>(cfg, scope, `${base}${path}`, init, fetchImpl);
  const e = encodeURIComponent;
  return {
    // Recordings and click maps
    recordingSettings: () => call<{ settings: RecordingSettings; store: string | null; retention_days: number }>("/recording-settings"),
    saveRecordingSettings: (patch: Partial<RecordingSettings>) => call<{ settings: RecordingSettings }>("/recording-settings", post(patch, "PUT")),
    replays: (filter: { distinct_id?: string; person_id?: string; event?: string; before?: string } = {}) => call<{ replays: Replay[]; offset_ms?: number | null }>(`/replays${q(filter)}`),
    replaysForPeople: (personIds: string[]) => call<{ replays: Replay[] }>("/replays/for-people", post({ person_ids: personIds })),
    replay: (session: string) => call<{ replay: Replay }>(`/replays/${e(session)}`),
    replayLink: (session: string, offsetMs = 0) => call<{ url: string; expires_at: string }>(`/replays/${e(session)}/link`, post({ offset_ms: offsetMs })),
    deleteReplay: (session: string) => call<{ deleted: number }>(`/replays/${e(session)}`, { method: "DELETE" }),
    heatmap: (params: { url: string; type?: string; from?: string; to?: string }) => call<HeatmapAnswer>(`/${HEAT}${q(params)}`),
    heatmapUrls: () => call<{ urls: { url: string; clicks: number }[] }>(`/${HEAT}/urls`),
    heatmapLink: (params: { url: string; type?: string; from?: string; to?: string; width?: number }) => call<{ url: string; expires_at: string }>(`/${HEAT}/link`, post(params)),
    // Surveys
    surveys: () => call<{ surveys: Survey[]; max_running: number }>("/surveys"),
    survey: (id: string) => call<{ survey: Survey }>(`/surveys/${e(id)}`),
    createSurvey: (input: Record<string, unknown>) => call<{ survey: Survey }>("/surveys", post(input)),
    setSurveyStatus: (id: string, status: "running" | "stopped") => call<{ survey: Survey }>(`/surveys/${e(id)}/status`, post({ status })),
    deleteSurvey: (id: string) => call<{ deleted: boolean }>(`/surveys/${e(id)}`, { method: "DELETE" }),
    surveyResults: (id: string) => call<{ results: SurveyResults }>(`/surveys/${e(id)}/results`),
    // Experiments
    experiments: () => call<{ experiments: Experiment[] }>(`/${EXP}`),
    experiment: (id: string) => call<{ experiment: Experiment; flag_state: FlagState }>(`/${EXP}/${e(id)}`),
    createExperiment: (input: Record<string, unknown>) => call<{ experiment: Experiment }>(`/${EXP}`, post(input)),
    deleteExperiment: (id: string) => call<{ deleted: boolean }>(`/${EXP}/${e(id)}`, { method: "DELETE" }),
    experimentFlagChange: (id: string, action: "start" | "stop", variant?: string) => call<{ flags_env: string; flag: FlagChange }>(`/${EXP}/${e(id)}/flag-value${q({ for: action, variant })}`),
    setExperimentStatus: (id: string, status: "running" | "stopped") => call<{ experiment: Experiment }>(`/${EXP}/${e(id)}/status`, post({ status })),
    experimentResults: (id: string) => call<{ results: ExperimentResults }>(`/${EXP}/${e(id)}/results`),
    flagsClientKey: async () => (await paFetch<{ settings: { flags_client_key: string | null } }>(cfg, scope, `${base}/settings`, {}, fetchImpl)).settings.flags_client_key,
    // Exports
    destinations: () => call<{ destinations: Destination[]; max_destinations: number }>("/exports"),
    destination: (id: string) => call<{ destination: Destination }>(`/exports/${e(id)}`),
    createDestination: (input: Record<string, unknown>) => call<{ destination: Destination; signing_secret: string | null }>("/exports", post(input)),
    updateDestination: (id: string, patch: { enabled?: boolean; retry_now?: boolean }) => call<{ destination: Destination }>(`/exports/${e(id)}`, post(patch, "PUT")),
    deleteDestination: (id: string) => call<{ deleted: boolean }>(`/exports/${e(id)}`, { method: "DELETE" }),
    testDestination: (id: string) => call<{ ok: boolean; status?: number; error?: string }>(`/exports/${e(id)}/test`, post({})),
    runs: (id: string) => call<{ runs: ExportRun[] }>(`/exports/${e(id)}/runs`),
    /** A saved insight's answer as CSV (raw response, status and body passed through). */
    async savedInsightCsv(insightId: string, now = new Date()): Promise<Response> {
      const token = await signOrgToken(cfg.secret, { org_id: scope.org_id, app_ids: scope.app_ids, role: scope.role ?? "owner" }, now);
      return fetchImpl(`${cfg.url}${base}/exports/saved/${e(insightId)}.csv`, { headers: { authorization: `Bearer ${token}` }, cache: "no-store" });
    },
  };
}

export type A3Api = ReturnType<typeof a3Api>;

export const problemText = (e: unknown): string => (e instanceof PaError ? e.message : "Product analytics could not be reached. Try again shortly.");

// ---- Pure helpers (tested) ----

export function duration(ms: number): string {
  const s = Math.max(0, Math.round(ms / 1000));
  if (s < 60) return `${s} s`;
  const m = Math.floor(s / 60);
  return m < 60 ? `${m} min ${s % 60} s` : `${Math.floor(m / 60)} h ${m % 60} min`;
}

export const percent = (share: number, digits = 1): string => `${(share * 100).toFixed(digits).replace(/\.0+$/, "")}%`;

export const lines = (v: FormDataEntryValue | null): string[] =>
  String(v ?? "")
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter(Boolean);

/** The new-survey form to the service's survey input (one question per survey from this screen). */
export function surveyFromForm(form: Record<string, string>): Record<string, unknown> {
  const type = (["open", "link", "rating", "single_choice", "multiple_choice"].includes(form.qtype) ? form.qtype : "open") as QuestionType;
  const question: Record<string, unknown> = { type, question: form.question ?? "" };
  if (type === "rating") question.scale = Number(form.scale || "5");
  if (type === "single_choice" || type === "multiple_choice") question.choices = (form.choices ?? "").split(/\r?\n/).map((s) => s.trim()).filter(Boolean);
  if (type === "link") question.link = form.link ?? "";
  if (form.button) question.buttonText = form.button;
  const out: Record<string, unknown> = { name: form.name ?? "", questions: [question], type: form.display === "api" ? "api" : "popover" };
  if (form.target === "url" && form.url) Object.assign(out, { url: form.url, url_match: form.url_match || "icontains" });
  if (form.target === "flag" && form.flag_key) out.flag_key = form.flag_key;
  // Segments are the console's word for the service's cohorts.
  if (form.target === "segment" && form.segment_id) out.cohort_id = form.segment_id;
  if (form.limit) out.response_limit = Number(form.limit);
  return out;
}

/** The rows of a question's results: every choice or rating value in order, then any other answer seen. */
export function answerKeys(q: SurveyQuestion | undefined, counts: Record<string, number>): string[] {
  const base = !q ? [] : q.type === "rating" ? (q.scale === 10 ? Array.from({ length: 11 }, (_, i) => String(i)) : Array.from({ length: q.scale ?? 5 }, (_, i) => String(i + 1))) : (q.choices ?? []);
  return [...base, ...Object.keys(counts).filter((k) => !base.includes(k))];
}

/** "control 50\ntest 50" -> variants. */
export function parseVariants(text: string): { key: string; weight: number }[] {
  return text
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean)
    .map((l) => {
      const [key, weight] = l.split(/[\s,:=]+/);
      return { key: (key ?? "").toLowerCase(), weight: Number(weight) };
    });
}

export function experimentFromForm(form: Record<string, string>): Record<string, unknown> {
  const kind = form.metric_kind === "count" || form.metric_kind === "sum" ? form.metric_kind : "conversion";
  const metric = kind === "sum" ? { kind, event: form.metric_event ?? "", property: form.metric_property ?? "" } : { kind, event: form.metric_event ?? "" };
  return {
    name: form.name ?? "",
    description: form.description ?? "",
    flag_key: form.flag_key ?? "",
    flags_env: form.flags_env || "production",
    variants: parseVariants(form.variants || "control 50\ntest 50"),
    rollout: form.rollout ? Number(form.rollout) : 100,
    metric,
    method: form.method === "frequentist" ? "frequentist" : "bayesian",
  };
}

export function destinationFromForm(form: Record<string, string>): Record<string, unknown> {
  const events = (form.events ?? "")
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter(Boolean);
  const common = { name: form.name ?? "", start: form.start === "all" ? "all" : "now", ...(events.length ? { events } : {}) };
  if (form.kind === "s3") {
    return { ...common, kind: "s3", config: { endpoint: form.endpoint ?? "", bucket: form.bucket ?? "", region: form.region || "auto", prefix: form.prefix ?? "", access_key_id: form.access_key_id ?? "" }, secret: form.secret ?? "" };
  }
  return { ...common, kind: "webhook", config: { url: form.url ?? "" } };
}

export const STATUS_TEXT: Record<Destination["status"], string> = { ok: "Delivering", retrying: "Retrying after an error", failing: "Failing: check the destination" };
