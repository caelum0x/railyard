// Remote config sections (A3; handed over by A1). Each provider returns the
// keys of its section; the core provider's `false` stays when a section is off.
//   replay:   sessionRecording {...} when the project records and a store is set up
//   heatmaps: heatmaps true when the project collects click maps
//   surveys:  surveys true when the project has a running survey (the SDK then reads /api/surveys/)
// The providers read the project's A3 settings through the service's
// database, bound once by the replay router when the service is built.

import type { Db } from "../db.ts";
import { replayStore } from "../replay/object-store.ts";
import { getRecordingSettings } from "../replay/settings.ts";
import type { ConfigProvider } from "./registry.ts";

let bound: Db | null = null;

/** Called by the replay router with the service's database. */
export function bindSections(db: Db): void {
  bound = db;
}

/** Masking the project cannot relax from the app: every input, [data-private] and .ph-mask text, [data-block] blocked. */
export const RECORDING_MASKING = { maskAllInputs: true, maskTextSelector: "[data-private], .ph-mask", blockSelector: "[data-block]" };

export const replayProvider: ConfigProvider = async ({ site }) => {
  if (!bound || !replayStore() || site.paused) return {};
  const s = await getRecordingSettings(bound, site.project_id);
  if (!s.replay_enabled) return {};
  return {
    sessionRecording: {
      endpoint: "/s/",
      consoleLogRecordingEnabled: false,
      recorderVersion: "v2",
      sampleRate: s.replay_sample_rate.toFixed(2),
      minimumDurationMilliseconds: s.replay_min_duration_ms || null,
      linkedFlag: null,
      networkPayloadCapture: null,
      masking: RECORDING_MASKING,
      urlTriggers: [],
      urlBlocklist: [],
      eventTriggers: [],
      recordCanvas: false,
      canvasFps: null,
      canvasQuality: null,
    },
  };
};

export const heatmapsProvider: ConfigProvider = async ({ site }) => {
  if (!bound || site.paused) return {};
  return (await getRecordingSettings(bound, site.project_id)).heatmaps_enabled ? { heatmaps: true } : {};
};

export const surveysProvider: ConfigProvider = async ({ site }) => {
  if (!bound || site.paused) return {};
  const n = await bound.asProject(site.project_id, async (c) => (await c.query<{ n: number }>("select count(*)::int as n from pa.surveys where project_id = $1 and status = 'running'", [site.project_id])).rows[0].n);
  return n > 0 ? { surveys: true } : {};
};

export const SECTION_PROVIDERS: ConfigProvider[] = [replayProvider, heatmapsProvider, surveysProvider];
