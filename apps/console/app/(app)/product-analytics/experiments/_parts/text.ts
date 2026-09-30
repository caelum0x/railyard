// Copy of the flag-based screens (shown only when RAILYARD_FLAGS_LIVE is on).
import type { FlagState } from "../../_lib/a3-client.ts";

export function flagStateText(s: FlagState): string {
  return {
    live: "The flag carries this experiment.",
    waiting: "The flag change is on its way (flags are cached for up to 30 seconds).",
    missing: "The flag does not carry this experiment.",
    not_connected: "Connect this app's flags environment on the Setup screen to start an experiment.",
  }[s];
}
