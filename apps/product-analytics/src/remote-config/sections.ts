// Remote config sections owned by A3 (handed over by A1: "then"). Each
// provider returns the keys of its section, for example
//   replay:   { sessionRecording: {...} }
//   heatmaps: { heatmaps: true }
//   surveys:  { surveys: true }
// Until A3 fills them they add nothing, so the core provider's `false` stays.

import type { ConfigProvider } from "./registry.ts";

export const replayProvider: ConfigProvider = () => ({});
export const heatmapsProvider: ConfigProvider = () => ({});
export const surveysProvider: ConfigProvider = () => ({});

export const SECTION_PROVIDERS: ConfigProvider[] = [replayProvider, heatmapsProvider, surveysProvider];
