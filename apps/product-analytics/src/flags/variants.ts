// Multivariate answers for experiments (A3). A flag whose value is an
// experiment value (see ../experiments/flag-value.ts) answers a variant per
// distinct_id with the SDKs' documented hashing, so the SDK's local
// evaluation, the service's /flags answer and the experiment's stats agree:
//   hash = parseInt(sha1("<flag>.<distinct_id><salt>")[0:15], 16) / 0xfffffffffffffff
//   in the rollout when hash("") <= rollout / 100
//   variant = the bucket [min, max) of the cumulative weights holding hash("variant")
// (the same arithmetic as the SDK core's getFeatureFlagHash and
// getFeatureFlagVariant, MIT). Any other value keeps the flag plain: null.

import { createHash } from "node:crypto";
import { parseFlagValue, type ExperimentValue } from "../experiments/flag-value.ts";
import type { SlotFlag } from "./client.ts";

export const LONG_SCALE = 0xfffffffffffffff;

export function flagHash(key: string, distinctId: string, salt = ""): number {
  const hex = createHash("sha1").update(`${key}.${distinctId}${salt}`, "utf8").digest("hex");
  return parseInt(hex.slice(0, 15), 16) / LONG_SCALE;
}

const parsed = new Map<string, ExperimentValue | null>();

function experimentOf(value: string | null): ExperimentValue | null {
  if (!value) return null;
  if (parsed.has(value)) return parsed.get(value) ?? null;
  const v = parseFlagValue(value);
  if (parsed.size > 1000) parsed.clear();
  parsed.set(value, v);
  return v;
}

export function variantOf(flagKey: string, v: ExperimentValue, distinctId: string): string | null {
  if (flagHash(flagKey, distinctId) > v.rollout / 100) return null;
  const h = flagHash(flagKey, distinctId, "variant");
  let min = 0;
  for (const variant of v.variants) {
    const max = min + variant.weight / 100;
    if (h >= min && h < max) return variant.key;
    min = max;
  }
  return null;
}

/** The variant key `distinctId` gets for `flagKey`, or null for a plain flag (or outside the rollout). */
export function variantFor(flagKey: string, flag: SlotFlag, distinctId: string): string | null {
  if (!flag.enabled) return null;
  const v = experimentOf(flag.value);
  return v ? variantOf(flagKey, v, distinctId) : null;
}
