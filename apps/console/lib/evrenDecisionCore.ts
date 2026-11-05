/**
 * Vendored copy of the shared Evren `/v1/systemone` decision-gateway wrapper
 * (`~/products/ai-knowledges/evren/decision-core`, `@evren/decision-core`
 * package, commit `3e05d51`). That module centralizes the fetch +
 * AbortController + timeout + fail-open + response-parsing boilerplate that
 * 5 real consumers across the portfolio (this one included, `lib/eventAnomaly.ts`)
 * had each independently hand-rolled from scratch.
 *
 * Railyard console is its own standalone deployable repo (Cloudflare Workers
 * via `opennextjs-cloudflare`/`wrangler`, no shared workspace with
 * `ai-knowledges/`), so this is a vendored source copy rather than a package
 * dependency -- same reason decision-core itself ships as plain `.ts` with no
 * build step (`main`/`types` both point straight at `index.ts`). Keep this in
 * sync with the canonical module by hand if the gateway contract changes;
 * do not fork the logic itself.
 *
 * `lib/eventAnomaly.ts` is the sole consumer in this app.
 */

export const EVREN_SYSTEMONE_MODEL = "laya";
export const DEFAULT_SYSTEMONE_TIMEOUT_MS = 8_000;

export type GatewayLog = (event: string, detail?: Record<string, unknown>) => void;

export interface GatewayDeps {
  /** Origin of the Evren LLM gateway, e.g. https://evren-llmapi.ssyz.org.tr */
  readonly baseUrl: string;
  readonly apiKey: string;
  readonly fetch: typeof fetch;
  readonly log: GatewayLog;
  readonly timeoutMs?: number;
}

/**
 * A `score` question MUST carry an explicit, non-empty `criteria` level
 * list. This is enforced at the type level on purpose: the tuple type
 * `readonly [string, ...string[]]` makes it impossible to construct a
 * `ScoreQuestion` with an empty or missing `criteria`, because every real
 * consumer independently learned live that the gateway rejects a bare
 * `{ type: "score", instructions }` request outright.
 */
export interface ScoreQuestion {
  readonly type: "score";
  readonly instructions: string;
  readonly criteria: readonly [string, ...string[]];
}

export interface NoulQuestion {
  readonly type: "noul";
  readonly instructions: string;
}

export interface ChoiceQuestion {
  readonly type: "choice";
  readonly instructions: string;
  readonly criteria: Record<string, string>;
}

export type SystemOneQuestion = ScoreQuestion | NoulQuestion | ChoiceQuestion;
export type SystemOneQuestions = Record<string, SystemOneQuestion>;

export interface SystemOneAnswer {
  readonly score?: unknown;
  readonly noul?: unknown;
  readonly choice?: unknown;
}

export interface SystemOneResponseBody {
  readonly answers?: Record<string, SystemOneAnswer>;
}

/**
 * Fail-open: any network error, timeout, non-200, or malformed body returns
 * `null` and logs through `deps.log` -- never throws. Every real consumer so
 * far treats this gateway as an additive signal on top of its own
 * deterministic floor, so a gateway outage or bad response must never be
 * able to block or throw out of the caller's own primary path.
 */
export async function callSystemOne(
  deps: GatewayDeps,
  state: Record<string, unknown>,
  questions: SystemOneQuestions,
): Promise<SystemOneResponseBody | null> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), deps.timeoutMs ?? DEFAULT_SYSTEMONE_TIMEOUT_MS);
  try {
    const res = await deps.fetch(`${deps.baseUrl}/v1/systemone`, {
      method: "POST",
      headers: { "content-type": "application/json", authorization: `Bearer ${deps.apiKey}` },
      body: JSON.stringify({ model: EVREN_SYSTEMONE_MODEL, state, questions }),
      signal: controller.signal,
    });
    if (!res.ok) {
      deps.log("evren_gateway.http_error", { status: res.status });
      return null;
    }
    return (await res.json()) as SystemOneResponseBody;
  } catch (error) {
    deps.log("evren_gateway.failed", { error: error instanceof Error ? error.message : String(error) });
    return null;
  } finally {
    clearTimeout(timer);
  }
}

export function clamp01(value: number): number {
  return Math.min(1, Math.max(0, value));
}

/**
 * Parses a `score` answer back to 0..1. A `score` question answers with a
 * continuous position across its `criteria` levels (index `0..length-1`,
 * interpolated), never already-normalized `0..1`. Returns `null` when the
 * answer is missing or not a finite number, so callers can fail open the
 * same way `callSystemOne` does on a transport error.
 */
export function parseScoreAnswer(body: SystemOneResponseBody, key: string, levelCount: number): number | null {
  const raw = body.answers?.[key]?.score;
  if (typeof raw !== "number" || !Number.isFinite(raw)) return null;
  return clamp01(raw / Math.max(1, levelCount - 1));
}

/** Parses a `noul` answer to 0..1. */
export function parseNoulAnswer(body: SystemOneResponseBody, key: string): number | null {
  const raw = body.answers?.[key]?.noul;
  if (typeof raw !== "number" || !Number.isFinite(raw)) return null;
  return clamp01(raw);
}

/** Parses a `choice` answer against the caller's own fixed, human-reviewed code list. */
export function parseChoiceAnswer<Code extends string>(body: SystemOneResponseBody, key: string, validCodes: readonly Code[]): Code | null {
  const raw = body.answers?.[key]?.choice;
  return typeof raw === "string" && (validCodes as readonly string[]).includes(raw) ? (raw as Code) : null;
}
