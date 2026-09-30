// Multivariate answers for experiments. Handed to A3 (then): until A3 fills
// it, every flag is plain on/off with its value, so this returns null.

import type { SlotFlag } from "./client.ts";

/** The variant key `distinctId` gets for `flagKey`, or null for a plain flag. */
export function variantFor(_flagKey: string, _flag: SlotFlag, _distinctId: string): string | null {
  return null;
}
