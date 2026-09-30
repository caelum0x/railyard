// Server-only client for the product analytics service. Every call carries
// a fresh org token scoped to the org's own app ids.

import { signOrgToken, type Role } from "./token.ts";

export interface PaConfig {
  url: string;
  secret: string;
}

/** Null when the service is not connected on this deployment (the page says so). */
export function paConfig(env: Record<string, string | undefined> = process.env): PaConfig | null {
  const url = env.PRODUCT_ANALYTICS_URL?.trim();
  const secret = env.PRODUCT_ANALYTICS_CONSOLE_SECRET?.trim();
  if (!url || !secret || secret.length < 32) return null;
  return { url: url.replace(/\/$/, ""), secret };
}

export interface Scope {
  org_id: string;
  app_ids: string[];
  role?: Role;
}

export class PaError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.status = status;
  }
}

export async function paFetch<T>(cfg: PaConfig, scope: Scope, path: string, init: RequestInit = {}, fetchImpl: typeof fetch = fetch, now = new Date()): Promise<T> {
  const token = await signOrgToken(cfg.secret, { org_id: scope.org_id, app_ids: scope.app_ids, role: scope.role ?? "owner" }, now);
  const res = await fetchImpl(`${cfg.url}${path}`, {
    ...init,
    headers: { authorization: `Bearer ${token}`, "content-type": "application/json", ...(init.headers ?? {}) },
    cache: "no-store",
  });
  const text = await res.text();
  let body: unknown = null;
  try {
    body = text ? JSON.parse(text) : null;
  } catch {
    body = null;
  }
  if (!res.ok) {
    const msg = body && typeof body === "object" && "error" in body ? String((body as { error: unknown }).error) : "Product analytics could not be reached. Try again shortly.";
    throw new PaError(res.status, msg);
  }
  return body as T;
}

/** Raw response (for downloads). */
export async function paRaw(cfg: PaConfig, scope: Scope, path: string, fetchImpl: typeof fetch = fetch, now = new Date()): Promise<Response> {
  const token = await signOrgToken(cfg.secret, { org_id: scope.org_id, app_ids: scope.app_ids, role: scope.role ?? "owner" }, now);
  return fetchImpl(`${cfg.url}${path}`, { headers: { authorization: `Bearer ${token}` }, cache: "no-store" });
}

export interface AppRef {
  id: string;
  name: string;
  site_key: string;
}

/** The app named in `?app=` if it is the org's, else the org's first app. */
export function pickApp(apps: AppRef[], name: string | undefined): AppRef | null {
  return apps.find((a) => a.name === name) ?? apps[0] ?? null;
}

export interface Status {
  project: { id: string; name: string };
  paused: boolean;
  first_event: { uuid: string; event: string; at: string } | null;
  usage: { month: string; received: number; stored: number; capped: number; dropped: number };
  org_stored_this_month: number;
  events_limit: number;
  cap_message: string | null;
  retention_days: number;
}

export interface FeedEvent {
  uuid: string;
  event: string;
  distinct_id: string;
  timestamp: string;
  url: string | null;
  screen: string | null;
  lib: string | null;
}

export interface PersonRow {
  id: string;
  properties: Record<string, unknown>;
  is_identified: boolean;
  created_at: string;
  distinct_ids: string[] | null;
}

export interface PrivacyJob {
  id: string;
  distinct_id: string;
  requested_at: string;
  due_at: string;
  status: "queued" | "running" | "done" | "failed";
  finished_at: string | null;
  result: Record<string, number> | null;
  error: string | null;
}

export interface Settings {
  require_consent: boolean;
  autocapture: boolean;
  capture_pageview: boolean;
  flags_client_key: string | null;
  retention_days: number | null;
}

export const fmtTime = (iso: string | null | undefined) => (iso ? new Date(iso).toISOString().replace("T", " ").slice(0, 16) + " UTC" : "");
