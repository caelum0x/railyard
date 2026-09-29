// Railyard Watch service (Wave P PY2.2): checks, the agent's list and
// results, state changes with owner email, and the public status view.

import { access } from "../ladder.ts";
import { constantTimeEqual, randomToken } from "../crypto.ts";
import type { Repo } from "../repo.ts";
import type { Entitlement } from "../types.ts";
import type { Mailer } from "./mail.ts";
import type { WatchStore } from "./store.ts";
import {
  CHECK_NAME,
  FAILS_TO_DOWN,
  HISTORY,
  MAX_CHECKS_PER_ORG,
  SLUG,
  type AgentResult,
  type Check,
  type CheckState,
  type CheckStatus,
} from "./types.ts";

export interface WatchDeps {
  store: WatchStore;
  repo: Repo;
  mailer: Mailer;
  consoleUrl: string;
  /** The agent's bearer token (WATCH_AGENT_TOKEN); null keeps the agent API off. */
  agentToken: string | null;
  /** RAILYARD_WATCH_LIVE: off, nothing runs and nothing customer-visible is shown. */
  live: boolean;
}

/** The one answer while Watch is off (neutral copy: the slot is arriving). */
export const ARRIVING = "This slot is arriving (PY2.2). Nothing was changed.";

export class WatchError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

const IPV4 = /^\d{1,3}(\.\d{1,3}){3}$/;
const BLOCKED_SUFFIX = /(^|\.)(localhost|local|internal|intranet|lan|home|corp|arpa|invalid|test|example|onion|ereboslab\.com)$/i;

/** A public https URL we may check from our box, or the reason it is refused. */
export function checkUrl(raw: unknown): string | WatchError {
  if (typeof raw !== "string" || raw.length > 500) return new WatchError(400, "Give the https address of your app, for example https://shop.example.com.");
  let u: URL;
  try {
    u = new URL(raw.trim());
  } catch {
    return new WatchError(400, "That is not a web address. Give the https address of your app.");
  }
  if (u.protocol !== "https:") return new WatchError(400, "Only https addresses can be checked.");
  if (u.username || u.password) return new WatchError(400, "Remove the user name and password from the address.");
  if (u.port && u.port !== "443" && u.port !== "8443") return new WatchError(400, "Use the standard https port.");
  const host = u.hostname.toLowerCase();
  if (!host.includes(".") || IPV4.test(host) || host.startsWith("[") || BLOCKED_SUFFIX.test(host)) {
    return new WatchError(400, "Give the public name of your app (not an IP address or a private name).");
  }
  u.hash = "";
  return u.toString();
}

/** Whether an org may run checks now: an active plan, or the add-on with the uptime slot. */
export function watchAllowed(ent: Entitlement | null, now: Date): boolean {
  const a = access(ent, now);
  if (a.state !== "active" && a.state !== "cancelling" && a.state !== "trial") return false;
  return ent?.plan !== "addon" || (ent.addon_slots ?? []).includes("uptime");
}

function slugFor(app: string): string {
  const base = app.toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40) || "app";
  return `${base}-${randomToken(6).toLowerCase().replace(/[^a-z0-9]/g, "").slice(0, 6).padEnd(6, "0")}`;
}

export async function createCheck(
  deps: WatchDeps,
  input: { orgId: string; app: string; name?: unknown; url: unknown; ownerEmail: string | null },
): Promise<Check> {
  if (!deps.live) throw new WatchError(503, ARRIVING);
  const url = checkUrl(input.url);
  if (url instanceof WatchError) throw url;
  const name = typeof input.name === "string" && input.name.trim() ? input.name.trim() : input.app;
  if (!CHECK_NAME.test(name)) throw new WatchError(400, "The name can use letters, digits, spaces, dots, dashes and apostrophes (60 at most).");
  if (input.ownerEmail !== null && !/^[^\s@]{1,64}@[^\s@]{1,190}\.[^\s@]{2,}$/.test(input.ownerEmail)) throw new WatchError(400, "The owner email is not an address.");
  const existing = await deps.store.listChecks(input.orgId);
  const same = existing.find((c) => c.app === input.app && c.url === url);
  if (same) return same;
  if (existing.length >= MAX_CHECKS_PER_ORG) throw new WatchError(409, `An organization can have ${MAX_CHECKS_PER_ORG} checks.`);
  let slug = slugFor(input.app);
  for (let i = 0; i < 3 && (await deps.store.findCheckBySlug(slug)); i++) slug = slugFor(input.app);
  if (!SLUG.test(slug)) throw new WatchError(500, "Could not create the check. Try again.");
  return deps.store.createCheck({ org_id: input.orgId, app: input.app, name, url, slug, owner_email: input.ownerEmail, paused: false });
}

export function statusUrl(deps: Pick<WatchDeps, "consoleUrl">, check: Check): string {
  return `${deps.consoleUrl}/status/${check.slug}`;
}

export function agentAuthorized(deps: Pick<WatchDeps, "agentToken">, header: string | null): boolean {
  if (!deps.agentToken || !header?.startsWith("Bearer ")) return false;
  return constantTimeEqual(header.slice(7).trim(), deps.agentToken);
}

/** The agent's list: id and URL only (no customer text reaches the checker's config). */
export async function agentChecks(deps: WatchDeps, now: Date): Promise<{ id: string; url: string; interval_s: number }[]> {
  if (!deps.live) return [];
  const checks = await deps.store.activeChecks();
  const allowed = new Map<string, boolean>();
  const out: { id: string; url: string; interval_s: number }[] = [];
  for (const c of checks) {
    if (!allowed.has(c.org_id)) allowed.set(c.org_id, watchAllowed(await deps.repo.getEntitlement(c.org_id), now));
    if (allowed.get(c.org_id)) out.push({ id: c.id, url: c.url, interval_s: 60 });
  }
  return out;
}

function emptyStatus(checkId: string): CheckStatus {
  return { check_id: checkId, state: "waiting", consecutive_failures: 0, first_result_at: null, last_checked_at: null, last_change_at: null, alerted_state: null, history: [] };
}

/** The next status after one result (pure). */
export function nextStatus(prev: CheckStatus, r: AgentResult): CheckStatus {
  const failures = r.ok ? 0 : prev.consecutive_failures + 1;
  let state: CheckState = prev.state;
  if (r.ok) state = "up";
  else if (failures >= FAILS_TO_DOWN) state = "down";
  const history = [...prev.history, { at: r.at, ok: r.ok, ms: Math.max(0, Math.round(r.ms)) }].slice(-HISTORY);
  return {
    ...prev,
    state,
    consecutive_failures: failures,
    first_result_at: prev.first_result_at ?? r.at,
    last_checked_at: r.at,
    last_change_at: state !== prev.state ? r.at : prev.last_change_at,
    history,
  };
}

function alertText(check: Check, state: CheckState, statusPage: string): { subject: string; text: string; html: string } {
  const down = state === "down";
  const subject = down ? `${check.name} is down` : `${check.name} is back up`;
  const line = down
    ? `${check.name} did not answer our last two checks. We will email you again when it is back.`
    : `${check.name} answered our last check again.`;
  const text = `${line}\n\nStatus page: ${statusPage}\n`;
  const esc = (s: string) => s.replace(/[&<>"']/g, (ch) => `&#${ch.charCodeAt(0)};`);
  const html = `<p>${esc(line)}</p><p><a href="${esc(statusPage)}">Status page</a></p>`;
  return { subject, text, html };
}

function parseResult(raw: unknown): AgentResult | null {
  if (!raw || typeof raw !== "object") return null;
  const r = raw as Record<string, unknown>;
  if (typeof r.id !== "string" || !/^[0-9a-f-]{36}$/.test(r.id)) return null;
  if (typeof r.ok !== "boolean" || typeof r.at !== "string" || Number.isNaN(Date.parse(r.at))) return null;
  const ms = typeof r.ms === "number" && Number.isFinite(r.ms) ? r.ms : 0;
  return { id: r.id, ok: r.ok, at: new Date(r.at).toISOString(), ms };
}

/** Applies the agent's results in time order; emails the owner once per change. */
export async function recordResults(deps: WatchDeps, body: unknown, now: Date): Promise<{ accepted: number; ignored: number; emailed: number }> {
  if (!deps.live) throw new WatchError(503, ARRIVING);
  const list = (body as { results?: unknown })?.results;
  if (!Array.isArray(list) || list.length > 5000) throw new WatchError(400, "results must be a list of at most 5000 entries");
  const results = list.map(parseResult);
  const valid = results.filter((r): r is AgentResult => r !== null).sort((a, b) => (a.at < b.at ? -1 : 1));
  let accepted = 0;
  let emailed = 0;
  const skew = now.getTime() + 5 * 60_000;
  for (const r of valid) {
    if (Date.parse(r.at) > skew) continue;
    const check = await deps.store.getCheck(r.id);
    if (!check || check.paused) continue;
    const prev = (await deps.store.getStatus(r.id)) ?? emptyStatus(r.id);
    if (prev.last_checked_at && r.at <= prev.last_checked_at) continue;
    let next = nextStatus(prev, r);
    const alertable = next.state === "down" || (next.state === "up" && next.alerted_state === "down");
    if (next.state !== next.alerted_state && alertable && check.owner_email) {
      const page = statusUrl(deps, check);
      const msg = alertText(check, next.state, page);
      const sent = await deps.mailer.send({ to: check.owner_email, ...msg, idempotencyKey: `watch:${check.id}:${next.state}:${next.last_change_at}`, project: check.org_id });
      if (sent) {
        next = { ...next, alerted_state: next.state };
        emailed += 1;
      }
    } else if (next.state === "up" && next.alerted_state === null) {
      next = { ...next, alerted_state: "up" };
    }
    await deps.store.saveStatus(next);
    accepted += 1;
  }
  return { accepted, ignored: list.length - accepted, emailed };
}

export interface StatusView {
  name: string;
  state: CheckState;
  label: string;
  uptime: string | null;
  lastChecked: string | null;
  bars: { ok: boolean; at: string }[];
}

/** What /status/<slug> shows: live results only, the check's name, never its address. */
export function statusView(check: Check, status: CheckStatus | null): StatusView {
  const s = status ?? emptyStatus(check.id);
  const ok = s.history.filter((h) => h.ok).length;
  const label = s.state === "up" ? "Operational" : s.state === "down" ? "Down" : "Waiting for the first check";
  return {
    name: check.name,
    state: s.state,
    label,
    uptime: s.history.length ? `${((ok / s.history.length) * 100).toFixed(s.history.length >= 10 ? 1 : 0)}% of the last ${s.history.length} checks` : null,
    lastChecked: s.last_checked_at ? `${s.last_checked_at.slice(0, 16).replace("T", " ")} UTC` : null,
    bars: s.history.map((h) => ({ ok: h.ok, at: h.at })),
  };
}

/** The org's checks with their state, for the console and the bridge. */
export async function listWithState(deps: WatchDeps, orgId: string, app?: string) {
  const checks = (await deps.store.listChecks(orgId)).filter((c) => !app || c.app === app);
  return Promise.all(
    checks.map(async (c) => {
      const st = await deps.store.getStatus(c.id);
      return {
        id: c.id,
        app: c.app,
        name: c.name,
        url: c.url,
        paused: c.paused,
        state: st?.state ?? "waiting",
        first_result_at: st?.first_result_at ?? null,
        last_checked_at: st?.last_checked_at ?? null,
        status_url: statusUrl(deps, c),
      };
    }),
  );
}
