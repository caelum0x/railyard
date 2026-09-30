// The experiment's flag value: the JSON an experiment writes into its flag's
// `value` through the flags slot's own setFlag (never the flag tables):
//   {"rollout":100,"variants":[{"key":"control","weight":50},{"key":"test","weight":50}],"experiment":"<id>"}
// Apps on the plain flags SDK still read `enabled` and this JSON as the value.

export const VARIANT_KEY = /^[a-z0-9][a-z0-9_-]{0,39}$/;
export const MAX_VARIANTS = 10;
/** The flags slot's MAX_VALUE (wave-p2-railyard apps/console/lib/flags/types.ts). */
export const MAX_FLAG_VALUE = 2000;

export interface Variant {
  key: string;
  weight: number;
}

export interface ExperimentValue {
  rollout: number;
  variants: Variant[];
  experiment: string;
}

export class VariantError extends Error {}

/** Validates variants: 2-10 unique keys, one of them "control", whole weights summing to 100. */
export function checkVariants(raw: unknown): Variant[] {
  if (!Array.isArray(raw) || raw.length < 2 || raw.length > MAX_VARIANTS) throw new VariantError(`An experiment needs 2 to ${MAX_VARIANTS} variants.`);
  const out: Variant[] = raw.map((v, i) => {
    const o = (v ?? {}) as Record<string, unknown>;
    if (typeof o.key !== "string" || !VARIANT_KEY.test(o.key)) throw new VariantError(`Variant ${i + 1} needs a key of lowercase letters, digits, dashes or underscores.`);
    if (typeof o.weight !== "number" || !Number.isInteger(o.weight) || o.weight < 0 || o.weight > 100) throw new VariantError(`Variant ${o.key} needs a whole-number weight from 0 to 100.`);
    return { key: o.key, weight: o.weight };
  });
  if (new Set(out.map((v) => v.key)).size !== out.length) throw new VariantError("Variant keys must be different.");
  if (!out.some((v) => v.key === "control")) throw new VariantError("One variant must be called control.");
  if (out.reduce((a, v) => a + v.weight, 0) !== 100) throw new VariantError("Variant weights must add up to 100.");
  return out;
}

export function checkRollout(raw: unknown): number {
  if (raw === undefined) return 100;
  if (typeof raw !== "number" || !Number.isInteger(raw) || raw < 0 || raw > 100) throw new VariantError("Rollout must be a whole percentage from 0 to 100.");
  return raw;
}

export function buildFlagValue(v: ExperimentValue): string {
  const s = JSON.stringify({ rollout: v.rollout, variants: v.variants.map((x) => ({ key: x.key, weight: x.weight })), experiment: v.experiment });
  if (s.length > MAX_FLAG_VALUE) throw new VariantError("The variants do not fit in a flag value.");
  return s;
}

/** The experiment value inside a flag's value, or null when the value is anything else. */
export function parseFlagValue(value: string | null | undefined): ExperimentValue | null {
  if (!value || value[0] !== "{" || value.length > MAX_FLAG_VALUE) return null;
  let o: Record<string, unknown>;
  try {
    o = JSON.parse(value) as Record<string, unknown>;
  } catch {
    return null;
  }
  if (typeof o !== "object" || o === null || typeof o.experiment !== "string" || o.experiment.length === 0 || o.experiment.length > 100) return null;
  try {
    const variants = checkVariantsLoose(o.variants);
    return { rollout: checkRollout(o.rollout), variants, experiment: o.experiment };
  } catch {
    return null;
  }
}

// A running flag may carry a single winning variant at 100 (after the experiment stops).
function checkVariantsLoose(raw: unknown): Variant[] {
  if (!Array.isArray(raw) || raw.length < 1 || raw.length > MAX_VARIANTS) throw new VariantError("bad variants");
  const out = raw.map((v) => {
    const o = (v ?? {}) as Record<string, unknown>;
    if (typeof o.key !== "string" || !VARIANT_KEY.test(o.key) || typeof o.weight !== "number" || !Number.isInteger(o.weight) || o.weight < 0 || o.weight > 100) throw new VariantError("bad variant");
    return { key: o.key, weight: o.weight };
  });
  if (new Set(out.map((v) => v.key)).size !== out.length || out.reduce((a, v) => a + v.weight, 0) !== 100) throw new VariantError("bad weights");
  return out;
}
