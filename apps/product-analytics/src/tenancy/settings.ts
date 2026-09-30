// Per-project settings (pa.project_settings). Missing row = the defaults.

import type { Db } from "../db.ts";
import type { Site } from "./site-directory.ts";

export interface ProjectSettings {
  require_consent: boolean;
  autocapture: boolean;
  capture_pageview: boolean;
  flags_client_key: string | null;
  retention_days: number | null;
}

export const DEFAULT_SETTINGS: ProjectSettings = {
  require_consent: true,
  autocapture: true,
  capture_pageview: true,
  flags_client_key: null,
  retention_days: null,
};

export const FLAGS_KEY = /^ry_flags_[A-Za-z0-9_-]{24,64}$/;

export async function getSettings(db: Db, projectId: string): Promise<ProjectSettings> {
  return db.asProject(projectId, async (c) => {
    const r = await c.query<ProjectSettings>(
      "select require_consent, autocapture, capture_pageview, flags_client_key, retention_days from pa.project_settings where project_id = $1",
      [projectId],
    );
    return r.rows[0] ?? DEFAULT_SETTINGS;
  });
}

export class SettingsError extends Error {}

/** Validates a partial update; returns the full new settings. */
export function mergeSettings(cur: ProjectSettings, patch: unknown): ProjectSettings {
  if (typeof patch !== "object" || patch === null || Array.isArray(patch)) throw new SettingsError("Send the settings as an object.");
  const p = patch as Record<string, unknown>;
  const next = { ...cur };
  for (const k of ["require_consent", "autocapture", "capture_pageview"] as const) {
    if (k in p) {
      if (typeof p[k] !== "boolean") throw new SettingsError(`${k} must be true or false.`);
      next[k] = p[k] as boolean;
    }
  }
  if ("flags_client_key" in p) {
    const v = p.flags_client_key;
    if (v !== null && (typeof v !== "string" || !FLAGS_KEY.test(v))) throw new SettingsError("The flags key must be the environment's public ry_flags_ key.");
    next.flags_client_key = v as string | null;
  }
  if ("retention_days" in p) {
    const v = p.retention_days;
    if (v !== null && (typeof v !== "number" || !Number.isInteger(v) || v < 1 || v > 3650)) throw new SettingsError("retention_days must be a whole number of days.");
    next.retention_days = v as number | null;
  }
  return next;
}

export async function putSettings(db: Db, site: Site, s: ProjectSettings): Promise<ProjectSettings> {
  return db.asProject(site.project_id, async (c) => {
    await c.query(
      `insert into pa.project_settings (project_id, org_id, require_consent, autocapture, capture_pageview, flags_client_key, retention_days, updated_at)
       values ($1, $2, $3, $4, $5, $6, $7, now())
       on conflict (project_id) do update set require_consent = excluded.require_consent, autocapture = excluded.autocapture,
         capture_pageview = excluded.capture_pageview, flags_client_key = excluded.flags_client_key,
         retention_days = excluded.retention_days, updated_at = now()`,
      [site.project_id, site.org_id, s.require_consent, s.autocapture, s.capture_pageview, s.flags_client_key, s.retention_days],
    );
    return s;
  });
}
