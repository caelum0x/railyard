// Session replay config (2026-10-15): per-site settings stored alongside the
// rest of the site's KV entry (`site:<key>`), under the `replay` object. Pure
// helpers so the ingest route, the public /api/replay/config route and the
// settings form all agree on the same defaults and the same validation.
//
// Privacy-first defaults: replay is OFF until a site owner turns it on, every
// input is masked, every text node is masked (maskTextSelector "*" masks all
// of them), and sampling, session length and total size are all capped.

export const DEFAULT_SAMPLE_RATE = 0.1;
export const MIN_SAMPLE_RATE = 0.01;
export const MAX_SAMPLE_RATE = 1;

export const DEFAULT_MAX_SESSION_MS = 30 * 60_000; // 30 minutes
export const MIN_MAX_SESSION_MS = 60_000; // 1 minute
export const MAX_MAX_SESSION_MS = 2 * 60 * 60_000; // 2 hours

export const DEFAULT_MAX_BYTES = 5_000_000; // 5 MB of rrweb JSON per session
export const MIN_MAX_BYTES = 200_000;
export const MAX_MAX_BYTES = 20_000_000;

export const DEFAULT_SESSIONS_PER_MONTH = 500; // paid-tier default session cap
export const MIN_SESSIONS_PER_MONTH = 10;
export const MAX_SESSIONS_PER_MONTH = 20_000;

export const DEFAULT_MASK_TEXT_SELECTOR = "*";

/** The effective replay config for a site, with every default filled in. Never trusts partial/missing data from an older KV entry. */
export function resolveReplayConfig(site) {
  const r = site && typeof site === "object" && site.replay && typeof site.replay === "object" ? site.replay : {};
  return {
    enabled: r.enabled === true,
    sample_rate: clamp(numberOr(r.sample_rate, DEFAULT_SAMPLE_RATE), MIN_SAMPLE_RATE, MAX_SAMPLE_RATE),
    mask_all_inputs: r.mask_all_inputs !== false,
    mask_text_selector: typeof r.mask_text_selector === "string" && r.mask_text_selector.trim() ? r.mask_text_selector.trim().slice(0, 200) : DEFAULT_MASK_TEXT_SELECTOR,
    max_session_ms: clamp(numberOr(r.max_session_ms, DEFAULT_MAX_SESSION_MS), MIN_MAX_SESSION_MS, MAX_MAX_SESSION_MS),
    max_bytes: clamp(numberOr(r.max_bytes, DEFAULT_MAX_BYTES), MIN_MAX_BYTES, MAX_MAX_BYTES),
    sessions_per_month: clamp(numberOr(r.sessions_per_month, DEFAULT_SESSIONS_PER_MONTH), MIN_SESSIONS_PER_MONTH, MAX_SESSIONS_PER_MONTH),
  };
}

function numberOr(v, fallback) {
  return typeof v === "number" && Number.isFinite(v) ? v : fallback;
}

function clamp(v, min, max) {
  return Math.min(max, Math.max(min, v));
}

/** The subset of the config the public, unauthenticated `/api/replay/config` route may return to the page tag. No session data, nothing about other sites. */
export function publicReplayConfig(site) {
  const c = resolveReplayConfig(site);
  return {
    enabled: c.enabled,
    sample_rate: c.sample_rate,
    mask_all_inputs: c.mask_all_inputs,
    mask_text_selector: c.mask_text_selector,
    max_session_ms: c.max_session_ms,
    max_bytes: c.max_bytes,
  };
}

/**
 * Validates a settings-form submission (POST /d/<token>/replay/settings).
 * @returns {{ok: true, replay: object} | {ok: false, error: string}}
 */
export function parseReplaySettings(input) {
  const enabled = input?.enabled === "on" || input?.enabled === true || input?.enabled === "1";
  const sampleRaw = Number(input?.sample_rate);
  const sample_rate = Number.isFinite(sampleRaw) ? clamp(sampleRaw, MIN_SAMPLE_RATE, MAX_SAMPLE_RATE) : DEFAULT_SAMPLE_RATE;
  const mask_all_inputs = !(input?.mask_all_inputs === "off" || input?.mask_all_inputs === false || input?.mask_all_inputs === "0");
  const selector = typeof input?.mask_text_selector === "string" ? input.mask_text_selector.trim() : "";
  if (selector.length > 200) return { ok: false, error: "The mask selector is too long." };
  const mask_text_selector = selector || DEFAULT_MASK_TEXT_SELECTOR;
  return { ok: true, replay: { enabled, sample_rate, mask_all_inputs, mask_text_selector } };
}

/** How many of this UTC month's new sessions are still allowed, given `used` already started. */
export function sessionRoom(used, sessionsPerMonth) {
  return Math.max(0, Math.floor(sessionsPerMonth) - Math.max(0, Math.floor(used)));
}
