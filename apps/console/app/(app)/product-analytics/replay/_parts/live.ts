// Which of these screens are live on this deployment. The owner turns a
// module on with the same switches the copy denylist reads (policy/copy-lint):
//   RAILYARD_REPLAY_LIVE  recordings and click maps
//   RAILYARD_FLAGS_LIVE   the flags slot, and so the flag-based screens
// Until then the screens say they are arriving and name nothing else.

export interface LiveModules {
  replay: boolean;
  flags: boolean;
}

const on = (v: string | undefined) => v === "1" || v === "true";

export function liveModules(env: Record<string, string | undefined> = process.env): LiveModules {
  return { replay: on(env.RAILYARD_REPLAY_LIVE), flags: on(env.RAILYARD_FLAGS_LIVE) };
}

export const ARRIVING = "This screen is arriving on this deployment.";
