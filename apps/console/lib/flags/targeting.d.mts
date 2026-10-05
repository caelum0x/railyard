// Types of targeting.mjs (Wave Q QY1.3). Byte copies sit next to the copies of
// targeting.mjs in the console and in product analytics.

export type Operator = "exact" | "is_not" | "icontains" | "not_icontains" | "regex" | "not_regex" | "gt" | "gte" | "lt" | "lte" | "is_set" | "is_not_set";
export type Scalar = string | number | boolean;
export type PropertyCondition = { type: "property"; key: string; operator: Operator; value?: Scalar | Scalar[] };
export type Condition = PropertyCondition | { type: "cohort"; id: string } | { type: "segment"; key: string };
export interface Rule {
  conditions: Condition[];
  rollout: number;
  value?: string | null;
}
export interface Targeting {
  rules: Rule[];
}
export interface Segment {
  key: string;
  name: string;
  conditions: PropertyCondition[];
}
export type SegmentMap = Record<string, { conditions: Condition[] }>;
export interface TargetingContext {
  id?: string | null;
  properties?: Record<string, unknown>;
  cohorts?: string[];
}
export interface Evaluation {
  enabled: boolean;
  value: string | null;
  reason: "DISABLED" | "STATIC" | "TARGETING_MATCH" | "NO_MATCH";
  rule: number | null;
}
export interface AnsweredFlag {
  enabled: boolean;
  value: string | null;
  targeting?: { on: boolean; value: string | null; rules: Rule[] };
}

export declare const LONG_SCALE: number;
export declare const OPERATORS: readonly Operator[];
export declare const MAX_RULES: number;
export declare const MAX_CONDITIONS: number;
export declare const MAX_SEGMENTS: number;
export declare const MAX_TEXT: number;
export declare const MAX_LIST: number;
export declare const SEGMENT_KEY: RegExp;
export declare const PROPERTY_KEY: RegExp;
export declare function sha1Hex(text: string): string;
export declare function flagHash(key: string, id: string, salt?: string): number;
export declare function parseTargeting(raw: unknown, segments?: Set<string>): Targeting | null | string;
export declare function parseSegment(raw: unknown): Segment | string;
export declare function segmentRefs(targeting: Targeting | null | undefined): Set<string>;
export declare function matchProperty(cond: PropertyCondition, properties: Record<string, unknown> | null | undefined): boolean;
export declare function evaluateFlag(key: string, flag: { enabled: boolean; value: string | null; targeting?: Targeting | null }, ctx?: TargetingContext, segments?: SegmentMap): Evaluation;
export declare function answerFlag(key: string, flag: { enabled: boolean; value: string | null; targeting?: Targeting | null }, ctx?: TargetingContext, segments?: SegmentMap): AnsweredFlag;
export declare function evaluateAnswer(key: string, answered: AnsweredFlag | null | undefined, ctx?: TargetingContext, segments?: SegmentMap): Evaluation;
export declare function referencedSegments(flags: Record<string, AnsweredFlag>, segments: SegmentMap): SegmentMap;
