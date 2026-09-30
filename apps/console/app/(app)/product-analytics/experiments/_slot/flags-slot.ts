// The experiment screen's only way to change a flag: the flags slot's own
// setFlag (Wave P PY2.1, apps/console/lib/flags/service.ts). Product analytics
// never writes flag tables and never asks the service to.
//
// flags merge pending: the slot lives on branch wave-p2-railyard and is not on
// main yet. The types below are copied from its lib/flags/types.ts, and
// `connectedSlot()` answers null until the merge; afterwards it becomes:
//
//   import { deps } from "../../../../../lib/deps.ts";
//   import { setFlag } from "../../../../../lib/flags/service.ts";
//   export function connectedSlot(): FlagsSlot | null {
//     const f = deps().flags;
//     if (!f) return null;
//     return {
//       envForClientKey: async (orgId, key) => { const e = await f.store.findEnvByClientKey(key); return e && e.org_id === orgId ? e : null; },
//       setFlag: (env, input, actor, now) => setFlag(f, env, input, actor, now),
//     };
//   }

import type { A3Api } from "../../_lib/a3-client.ts";

/** wave-p2-railyard apps/console/lib/flags/types.ts (copied). */
export interface FlagEnv {
  id: string;
  org_id: string;
  app: string;
  env: string;
  client_key: string;
}

/** wave-p2-railyard apps/console/lib/flags/types.ts (copied). */
export interface Flag {
  key: string;
  enabled: boolean;
  value: string | null;
  description: string | null;
  updated_at: string | null;
}

/** The two slot functions the experiment screen uses. */
export interface FlagsSlot {
  envForClientKey(orgId: string, clientKey: string): Promise<FlagEnv | null>;
  /** lib/flags/service.ts setFlag(deps, env, raw, actor, now), with the slot's deps bound. */
  setFlag(env: FlagEnv, input: { key: string; enabled: boolean; value: string | null }, actor: string, now: Date): Promise<Flag>;
}

export function connectedSlot(): FlagsSlot | null {
  return null;
}

export class ExperimentChangeError extends Error {}

/**
 * Starts or stops an experiment: asks the service for the flag change, makes
 * it through the slot's setFlag on the app's flags environment, then records
 * the new status. Nothing else writes the flag.
 */
export async function changeExperiment(
  api: A3Api,
  slot: FlagsSlot | null,
  orgId: string,
  experimentId: string,
  action: "start" | "stop",
  keep: string | undefined,
  actor: string,
  now: Date,
): Promise<Flag> {
  if (!slot) throw new ExperimentChangeError("Flags are not connected on this deployment yet, so experiments cannot start.");
  const key = await api.flagsClientKey();
  if (!key) throw new ExperimentChangeError("Connect this app's flags environment on the Setup screen first.");
  const env = await slot.envForClientKey(orgId, key);
  if (!env) throw new ExperimentChangeError("The app's flags key does not belong to a flags environment of this organization.");
  const change = await api.experimentFlagChange(experimentId, action, keep);
  if (change.flags_env !== env.env) throw new ExperimentChangeError(`This experiment runs in the ${change.flags_env} flags environment, but the app is connected to ${env.env}.`);
  const flag = await slot.setFlag(env, change.flag, actor, now);
  await api.setExperimentStatus(experimentId, action === "start" ? "running" : "stopped");
  return flag;
}
