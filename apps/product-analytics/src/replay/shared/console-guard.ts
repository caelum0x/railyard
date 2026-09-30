// The console side of every A3 API (replays, heatmaps, surveys, experiments,
// exports): the console's 5-minute org token, the project must be one of its
// app ids and belong to its org; viewers read, other roles also write. Errors
// are plain sentences; unexpected ones are logged and answered generically.

import type { Context, Hono, MiddlewareHandler } from "hono";
import { isUuid } from "../../db.ts";
import type { Logger } from "../../log.ts";
import { logPath } from "../../log.ts";
import { verifyConsoleToken, type ConsoleClaims } from "../../tenancy/console-token.ts";
import type { Site, SiteDirectory } from "../../tenancy/site-directory.ts";

export type ConsoleEnv = { Variables: { claims: ConsoleClaims; site: Site } };
export type CC = Context<ConsoleEnv>;

export const MAX_JSON_BYTES = 256 * 1024;

/** A refusal the caller can act on: answered with its status and message. */
export class ApiError extends Error {
  status: 400 | 403 | 404 | 409 | 413 | 429 | 503;
  constructor(status: ApiError["status"], message: string) {
    super(message);
    this.status = status;
  }
}

export interface GuardDeps {
  consoleSecret: string | null;
  sites: SiteDirectory;
  now: () => Date;
  log: Logger;
}

export function consoleGuard(deps: GuardDeps): MiddlewareHandler<ConsoleEnv> {
  return async (c, next) => {
    if (!deps.consoleSecret) return c.json({ error: "The console connection is not configured on this service." }, 503);
    const auth = c.req.header("authorization") ?? "";
    const claims = await verifyConsoleToken(deps.consoleSecret, auth.startsWith("Bearer ") ? auth.slice(7) : null, deps.now());
    if (!claims) return c.json({ error: "Sign in again." }, 401);
    const id = c.req.param("id") ?? "";
    if (!isUuid(id) || !claims.app_ids.includes(id)) return c.json({ error: "This app is not in your organization." }, 403);
    const site = await deps.sites.byProject(id);
    if (!site || site.org_id !== claims.org_id) return c.json({ error: "This app is not in your organization." }, 403);
    c.set("claims", claims);
    c.set("site", site);
    await next();
  };
}

/** Answers ApiError with its status; anything else is logged and answered with a generic 500. */
export function plainErrors(app: Hono<ConsoleEnv>, log: Logger, area: string): void {
  app.onError((err, c) => {
    if (err instanceof ApiError) return c.json({ error: err.message }, err.status);
    log.error(`${area} request failed`, { path: logPath(c.req.url), error: err.message });
    return c.json({ error: "Something went wrong. Try again shortly." }, 500);
  });
}

export function requireWriter(c: CC): void {
  if (c.get("claims").role === "viewer") throw new ApiError(403, "Your role can view this app but not change it.");
}

export async function jsonBody(c: Context, max = MAX_JSON_BYTES): Promise<Record<string, unknown>> {
  const text = await c.req.text();
  if (text.length > max) throw new ApiError(413, "The request is too large.");
  if (!text) return {};
  let v: unknown;
  try {
    v = JSON.parse(text);
  } catch {
    throw new ApiError(400, "Send JSON.");
  }
  if (typeof v !== "object" || v === null || Array.isArray(v)) throw new ApiError(400, "Send a JSON object.");
  return v as Record<string, unknown>;
}

export const projectOf = (c: CC): string => c.get("site").project_id;

export function uuidParam(c: Context, name: string, what: string): string {
  const v = c.req.param(name) ?? "";
  if (!isUuid(v)) throw new ApiError(404, `That ${what} does not exist.`);
  return v;
}

export function text(v: unknown, field: string, max: number, required = true): string | null {
  if (v === undefined || v === null || v === "") {
    if (required) throw new ApiError(400, `${field} is required.`);
    return null;
  }
  if (typeof v !== "string") throw new ApiError(400, `${field} must be text.`);
  const t = v.trim();
  if (required && !t) throw new ApiError(400, `${field} is required.`);
  if (t.length > max) throw new ApiError(400, `${field} must be at most ${max} characters.`);
  return t || null;
}
