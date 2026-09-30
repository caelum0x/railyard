// Per-project recording switches (pa.recording_settings). Missing row = the
// defaults: recording and heatmaps off until the project turns them on.

import type { Db } from "../db.ts";
import type { Site } from "../tenancy/site-directory.ts";
import { DEFAULT_SAMPLE_RATE } from "./limits.ts";
import { ApiError } from "./shared/console-guard.ts";

export interface RecordingSettings {
  replay_enabled: boolean;
  replay_sample_rate: number;
  replay_min_duration_ms: number;
  heatmaps_enabled: boolean;
}

export const DEFAULT_RECORDING: RecordingSettings = {
  replay_enabled: false,
  replay_sample_rate: DEFAULT_SAMPLE_RATE,
  replay_min_duration_ms: 0,
  heatmaps_enabled: false,
};

export async function getRecordingSettings(db: Db, projectId: string): Promise<RecordingSettings> {
  return db.asProject(projectId, async (c) => {
    const r = await c.query<{ replay_enabled: boolean; replay_sample_rate: string; replay_min_duration_ms: number; heatmaps_enabled: boolean }>(
      "select replay_enabled, replay_sample_rate, replay_min_duration_ms, heatmaps_enabled from pa.recording_settings where project_id = $1",
      [projectId],
    );
    const row = r.rows[0];
    return row ? { ...row, replay_sample_rate: Number(row.replay_sample_rate) } : DEFAULT_RECORDING;
  });
}

export function mergeRecordingSettings(cur: RecordingSettings, patch: Record<string, unknown>): RecordingSettings {
  const next = { ...cur };
  for (const k of ["replay_enabled", "heatmaps_enabled"] as const) {
    if (k in patch) {
      if (typeof patch[k] !== "boolean") throw new ApiError(400, `${k} must be true or false.`);
      next[k] = patch[k] as boolean;
    }
  }
  if ("replay_sample_rate" in patch) {
    const v = patch.replay_sample_rate;
    if (typeof v !== "number" || !(v >= 0 && v <= 1)) throw new ApiError(400, "The sample rate must be a number from 0 to 1.");
    next.replay_sample_rate = Math.round(v * 1000) / 1000;
  }
  if ("replay_min_duration_ms" in patch) {
    const v = patch.replay_min_duration_ms;
    if (typeof v !== "number" || !Number.isInteger(v) || v < 0 || v > 30_000) throw new ApiError(400, "The minimum duration must be 0 to 30000 milliseconds.");
    next.replay_min_duration_ms = v;
  }
  return next;
}

export async function putRecordingSettings(db: Db, site: Site, s: RecordingSettings): Promise<RecordingSettings> {
  await db.asProject(site.project_id, (c) =>
    c.query(
      `insert into pa.recording_settings (project_id, org_id, replay_enabled, replay_sample_rate, replay_min_duration_ms, heatmaps_enabled, updated_at)
       values ($1, $2, $3, $4, $5, $6, now())
       on conflict (project_id) do update set replay_enabled = excluded.replay_enabled, replay_sample_rate = excluded.replay_sample_rate,
         replay_min_duration_ms = excluded.replay_min_duration_ms, heatmaps_enabled = excluded.heatmaps_enabled, updated_at = now()`,
      [site.project_id, site.org_id, s.replay_enabled, s.replay_sample_rate, s.replay_min_duration_ms, s.heatmaps_enabled],
    ),
  );
  return s;
}
