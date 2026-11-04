import type { InsightsSummary } from "./insights.ts";

// Typed AI judgment of per-app event-volume anomaly severity, on top of (never
// instead of) the plain current/baseline arithmetic this module computes
// itself from two real insightsClient.summary() calls. Calls Evren's decision
// gateway (POST /v1/systemone, model "laya") -- same wire contract and module
// shape as done/big-projects/bidstage-tenders' match/evren-fit.ts,
// sahibinden's packages/db/src/repository/evren-risk.ts and homeplan's
// lib/architectFit.ts (read all three first): a pure request/response module
// with no env or fetch globals baked in, so the only caller (the Usage page,
// via lib/deps.ts) injects both. Today the Usage page only renders a bare
// current-period event count against the plan cap (lib/caps.ts's
// capMessage) -- there is no anomaly/spike/drop detection anywhere in this
// repo. This module is additive only: a gateway outage or bad response can
// never remove or block the existing event count or cap note.

export const EVENT_ANOMALY_MODEL = "laya";
export const DEFAULT_EVENT_ANOMALY_TIMEOUT_MS = 8_000;

export type EventAnomalyLog = (event: string, detail?: Record<string, unknown>) => void;

export type EventAnomalyDeps = {
  /** Origin of the Evren LLM gateway, e.g. https://evren-llmapi.ssyz.org.tr */
  readonly baseUrl: string;
  readonly apiKey: string;
  readonly fetch: typeof fetch;
  readonly log: EventAnomalyLog;
  readonly timeoutMs?: number;
};

/**
 * Fixed, reviewable reason labels. The model only ever picks one of these keys
 * (a `choice` question); the display text is ours, not generated, so every
 * label the Usage page badge shows is something a human already approved.
 */
export const EVENT_ANOMALY_REASON_LABELS = {
  severe_drop: "Event volume has collapsed far below this app's normal baseline.",
  notable_drop: "Event volume is meaningfully below this app's normal baseline, but not collapsed.",
  normal_variance: "Event volume is within the ordinary day-to-day range for this app's own baseline.",
  notable_spike: "Event volume is meaningfully above this app's normal baseline.",
  severe_spike: "Event volume is far above this app's normal baseline, an extreme spike.",
} as const;

export type EventAnomalyReasonCode = keyof typeof EVENT_ANOMALY_REASON_LABELS;

const REASON_CODES = Object.keys(EVENT_ANOMALY_REASON_LABELS) as readonly EventAnomalyReasonCode[];

export type EventAnomalyCategory = "normal_variance" | "traffic_spike" | "traffic_drop";

function categoryFromReasonCode(reasonCode: EventAnomalyReasonCode): EventAnomalyCategory {
  if (reasonCode === "notable_spike" || reasonCode === "severe_spike") return "traffic_spike";
  if (reasonCode === "notable_drop" || reasonCode === "severe_drop") return "traffic_drop";
  return "normal_variance";
}

export type EventAnomalyResult = {
  /** 0 (severe drop) .. 0.5 (normal) .. 1 (severe spike). */
  readonly position: number;
  /** How far from normal, 0..1, regardless of direction: abs(position - 0.5) * 2. */
  readonly severity: number;
  readonly reasonCode: EventAnomalyReasonCode;
  readonly reason: string;
  readonly category: EventAnomalyCategory;
};

/**
 * Per-app features computed in plain code from two real insightsClient.summary()
 * calls (current period + a trailing baseline period), never trusted to the
 * model. `ratio` and `hasBaseline` are the real ground truth every grounding
 * function below checks the model's claim against.
 */
export type EventAnomalyFeatures = {
  readonly appId: string;
  readonly currentEvents: number;
  readonly baselineMeanDaily: number;
  /** currentEvents / baselineMeanDaily, or null when hasBaseline is false. */
  readonly ratio: number | null;
  /** False when the trailing baseline period has zero events for this app (e.g. brand-new app) -- nothing real to compare against. */
  readonly hasBaseline: boolean;
  /** 0 = Sunday .. 6 = Saturday, UTC. */
  readonly dayOfWeek: number;
  readonly appAgeDays: number | null;
}

/** Builds per-app features from a current-period summary and a trailing-baseline summary over `baselineDays`. */
export function computeEventAnomalyFeatures(current: InsightsSummary, baseline: InsightsSummary, baselineDays: number, now: Date = new Date()): EventAnomalyFeatures[] {
  const baselineByApp = new Map(baseline.apps.map((a) => [a.app_id, a]));
  const days = Math.max(1, baselineDays);
  return current.apps.map((a) => {
    const b = baselineByApp.get(a.app_id) ?? null;
    const baselineMeanDaily = b ? b.events / days : 0;
    const hasBaseline = baselineMeanDaily > 0;
    const ratio = hasBaseline ? a.events / baselineMeanDaily : null;
    const firstEventIso = b?.first_event_at ?? a.first_event_at ?? null;
    const appAgeDays = firstEventIso ? Math.max(0, Math.floor((now.getTime() - new Date(firstEventIso).getTime()) / 86_400_000)) : null;
    return { appId: a.app_id, currentEvents: a.events, baselineMeanDaily, ratio, hasBaseline, dayOfWeek: now.getUTCDay(), appAgeDays };
  });
}

function state(features: EventAnomalyFeatures, currentPeriodDays: number, baselinePeriodDays: number): Record<string, unknown> {
  return {
    current_events: features.currentEvents,
    current_period_days: currentPeriodDays,
    baseline_mean_daily_events: Math.round(features.baselineMeanDaily * 100) / 100,
    baseline_period_days: baselinePeriodDays,
    ratio: features.ratio === null ? null : Math.round(features.ratio * 100) / 100,
    has_baseline: features.hasBaseline,
    day_of_week: features.dayOfWeek,
    app_age_days: features.appAgeDays,
  };
}

/**
 * A `score` question answers with a continuous position across these levels
 * (index 0..length-1, interpolated), never already-normalized 0..1 -- same
 * documented gotcha as evren-fit.ts's FIT_LEVELS / evren-risk.ts's RISK_LEVELS
 * / architectFit.ts's INTENT_LEVELS: a `score` question without an explicit
 * `criteria` level list is rejected outright by the real gateway.
 * {@link parseEventAnomaly} divides by `SEVERITY_LEVELS.length - 1` to get
 * back to a 0..1 position.
 */
const SEVERITY_LEVELS = [
  "severe drop: event volume has collapsed far below this app's normal baseline",
  "notable drop: meaningfully below baseline, but not collapsed",
  "normal variance: within the ordinary day-to-day range of this app's own baseline",
  "notable spike: meaningfully above baseline",
  "severe spike: event volume is far above this app's normal baseline, an extreme spike",
] as const;

const QUESTIONS = {
  severity: {
    type: "score",
    instructions:
      "Score how far this app's current event volume is from its own normal baseline, using the computed ratio (current_events relative to baseline_mean_daily_events, already computed in plain arithmetic) as the real evidence -- not a guess from current_events alone, since different apps have very different normal scales. A ratio near 1 (roughly 0.7-1.3) is ordinary variance, not a spike or drop. Treat has_baseline=false (a brand-new app with no real trailing history to compare against) as normal_variance regardless of current_events.",
    criteria: SEVERITY_LEVELS,
  },
  reason: {
    type: "choice",
    instructions:
      "Which single reason best explains the severity judgment above? Pick normal_variance whenever has_baseline is false or the ratio is close to 1.",
    criteria: Object.fromEntries(REASON_CODES.map((code) => [code, EVENT_ANOMALY_REASON_LABELS[code]])),
  },
} as const;

type DecisionBody = {
  readonly answers?: Record<string, { readonly score?: unknown; readonly choice?: unknown }>;
};

function clamp01(value: number): number {
  return Math.min(1, Math.max(0, value));
}

function toResult(position: number, reasonCode: EventAnomalyReasonCode): EventAnomalyResult {
  return {
    position,
    severity: Math.abs(position - 0.5) * 2,
    reasonCode,
    reason: EVENT_ANOMALY_REASON_LABELS[reasonCode],
    category: categoryFromReasonCode(reasonCode),
  };
}

/** Pure parse of a /v1/systemone response body; exported so callers never need a live network call to test it. */
export function parseEventAnomaly(body: DecisionBody): EventAnomalyResult | null {
  const answers = body.answers ?? {};
  const severityRaw = answers.severity?.score;
  const reasonRaw = answers.reason?.choice;
  if (typeof severityRaw !== "number" || !Number.isFinite(severityRaw)) return null;
  const reasonCode = typeof reasonRaw === "string" && (REASON_CODES as readonly string[]).includes(reasonRaw) ? (reasonRaw as EventAnomalyReasonCode) : null;
  if (!reasonCode) return null;
  const position = clamp01(severityRaw / (SEVERITY_LEVELS.length - 1));
  return toResult(position, reasonCode);
}

/**
 * Fail-open: any network error, timeout, non-200, or unparseable body returns
 * null and is logged, never thrown. The Usage page's real event count and
 * lib/caps.ts's capMessage are the sole authoritative path; this call can
 * only add an anomaly badge on top of them, so a gateway outage or bad
 * response can never break the Usage page.
 */
export async function classifyEventAnomaly(deps: EventAnomalyDeps, features: EventAnomalyFeatures, currentPeriodDays: number, baselinePeriodDays: number): Promise<EventAnomalyResult | null> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), deps.timeoutMs ?? DEFAULT_EVENT_ANOMALY_TIMEOUT_MS);
  try {
    const res = await deps.fetch(`${deps.baseUrl}/v1/systemone`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${deps.apiKey}` },
      body: JSON.stringify({ model: EVENT_ANOMALY_MODEL, state: state(features, currentPeriodDays, baselinePeriodDays), questions: QUESTIONS }),
      signal: controller.signal,
    });
    if (!res.ok) {
      deps.log("event_anomaly.http_error", { status: res.status, app_id: features.appId });
      return null;
    }
    const body = (await res.json()) as DecisionBody;
    const result = parseEventAnomaly(body);
    if (!result) deps.log("event_anomaly.unparseable_response", { app_id: features.appId });
    return result;
  } catch (error) {
    deps.log("event_anomaly.failed", { app_id: features.appId, error: error instanceof Error ? error.message : String(error) });
    return null;
  } finally {
    clearTimeout(timer);
  }
}

export type EventAnomalyEvidence = { readonly ratio: number | null; readonly hasBaseline: boolean };

export function buildEventAnomalyEvidence(features: EventAnomalyFeatures): EventAnomalyEvidence {
  return { ratio: features.ratio, hasBaseline: features.hasBaseline };
}

/** Ratio thresholds the grounding/finalize functions below check the model's claim against -- real arithmetic, never the model's own opinion. */
export const RATIO_NORMAL_LOW = 0.7;
export const RATIO_NORMAL_HIGH = 1.3;
export const RATIO_SEVERE_SPIKE = 2.0;
export const RATIO_SEVERE_DROP = 0.5;

/**
 * The position range each reasonCode is allowed to land in after a
 * ground/finalize correction, so a corrected reasonCode always reads back as
 * the category it actually implies -- same discipline as architectFit.ts's
 * clampScoreToTier (verified live there 2026-11-04: two real briefs landed on
 * the same tier with only the reasonCode telling them apart, before that fix).
 */
const POSITION_RANGE: Record<EventAnomalyReasonCode, readonly [number, number]> = {
  severe_drop: [0, 0.12],
  notable_drop: [0.12, 0.4],
  normal_variance: [0.4, 0.6],
  notable_spike: [0.6, 0.88],
  severe_spike: [0.88, 1],
};

function withReason(result: EventAnomalyResult, reasonCode: EventAnomalyReasonCode): EventAnomalyResult {
  const [lo, hi] = POSITION_RANGE[reasonCode];
  const position = Math.min(Math.max(result.position, lo), hi);
  return toResult(position, reasonCode);
}

/**
 * Downgrades a spike/drop reason the model claimed when the real computed
 * ratio (current_events / baseline_mean_daily_events, both plain arithmetic
 * over insightsClient.summary()'s own numbers, never trusted to the model)
 * does not support it -- same discipline as evren-fit.ts's groundAiFitReason
 * and evren-risk.ts's groundAiRiskReason. With no real trailing baseline for
 * this app (hasBaseline false, e.g. a brand-new app with zero events in the
 * baseline window), there is nothing real to compare against, so any
 * spike/drop claim is downgraded to normal_variance regardless of the
 * model's own confidence. Call this BEFORE {@link finalizeEventAnomalyReason}.
 */
export function groundEventAnomalyReason(result: EventAnomalyResult, evidence: EventAnomalyEvidence): EventAnomalyResult {
  if (!evidence.hasBaseline || evidence.ratio === null) {
    return result.reasonCode === "normal_variance" ? result : withReason(result, "normal_variance");
  }
  const ratio = evidence.ratio;
  const claimsSpike = result.reasonCode === "notable_spike" || result.reasonCode === "severe_spike";
  const claimsDrop = result.reasonCode === "notable_drop" || result.reasonCode === "severe_drop";
  if (claimsSpike && ratio <= RATIO_NORMAL_HIGH) return withReason(result, "normal_variance");
  if (claimsDrop && ratio >= RATIO_NORMAL_LOW) return withReason(result, "normal_variance");
  if (result.reasonCode === "severe_spike" && ratio < RATIO_SEVERE_SPIKE) return withReason(result, "notable_spike");
  if (result.reasonCode === "severe_drop" && ratio > RATIO_SEVERE_DROP) return withReason(result, "notable_drop");
  return result;
}

/**
 * Upgrades the opposite, internally-inconsistent case: the model answered
 * `normal_variance` while the real ratio is clearly outside the ordinary
 * range. Budgeted for and verified necessary to check live (every prior
 * Evren consumer of this gateway -- evren-fit.ts, evren-risk.ts and
 * architectFit.ts -- independently found at least one internally-
 * inconsistent score/reason pair on a real call; evren-risk.ts's
 * finalizeAiRiskReason is the closest precedent for this exact "a
 * contradictory reason must not suppress a signal the deterministic
 * evidence already supports" fix). Call AFTER {@link groundEventAnomalyReason},
 * which handles the opposite direction (an unsupported spike/drop claim).
 */
export function finalizeEventAnomalyReason(result: EventAnomalyResult, evidence: EventAnomalyEvidence): EventAnomalyResult {
  if (result.reasonCode !== "normal_variance" || !evidence.hasBaseline || evidence.ratio === null) return result;
  const ratio = evidence.ratio;
  if (ratio >= RATIO_SEVERE_SPIKE) return withReason(result, "severe_spike");
  if (ratio > RATIO_NORMAL_HIGH) return withReason(result, "notable_spike");
  if (ratio <= RATIO_SEVERE_DROP) return withReason(result, "severe_drop");
  if (ratio < RATIO_NORMAL_LOW) return withReason(result, "notable_drop");
  return result;
}

/** classifyEventAnomaly + both grounding passes in one call -- what the Usage page actually calls per app. Fail-open: null on any gateway problem. */
export async function classifyAppEventAnomaly(deps: EventAnomalyDeps, features: EventAnomalyFeatures, currentPeriodDays: number, baselinePeriodDays: number): Promise<EventAnomalyResult | null> {
  const raw = await classifyEventAnomaly(deps, features, currentPeriodDays, baselinePeriodDays);
  if (!raw) return null;
  const evidence = buildEventAnomalyEvidence(features);
  return finalizeEventAnomalyReason(groundEventAnomalyReason(raw, evidence), evidence);
}

/** The highest-severity real result across an org's apps, or null when every app is null/normal_variance -- what the Usage page's single badge shows. */
export function worstEventAnomaly(results: readonly (EventAnomalyResult | null)[]): EventAnomalyResult | null {
  let worst: EventAnomalyResult | null = null;
  for (const r of results) {
    if (!r || r.category === "normal_variance") continue;
    if (!worst || r.severity > worst.severity) worst = r;
  }
  return worst;
}
