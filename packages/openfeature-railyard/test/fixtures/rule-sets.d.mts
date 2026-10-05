import type { Condition, Segment, Targeting } from "../../src/targeting.mjs";

export declare const COHORT_A: string;
export declare const COHORT_B: string;
export declare const SEGMENTS: Record<string, Segment>;
export declare const RULE_SETS: Record<string, { enabled: boolean; value: string | null; cohort?: boolean; targeting: Targeting & { rules: { conditions: Condition[]; rollout: number; value?: string | null }[] } }>;
export declare function idOf(i: number): string;
export declare function propsOf(i: number, complete?: boolean): Record<string, string | number | boolean>;
export declare function cohortsOf(i: number): string[];
