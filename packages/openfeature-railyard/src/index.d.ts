export type Operator = "exact" | "is_not" | "icontains" | "not_icontains" | "regex" | "not_regex" | "gt" | "gte" | "lt" | "lte" | "is_set" | "is_not_set";
export type Condition =
  | { type: "property"; key: string; operator: Operator; value?: string | number | boolean | (string | number | boolean)[] }
  | { type: "cohort"; id: string }
  | { type: "segment"; key: string };
export interface Rule {
  conditions: Condition[];
  /** 0 to 100 (default 100): the share of matching people, by a stable hash of flag key and id. */
  rollout: number;
  /** A value for the people this rule serves (else the flag's value). */
  value?: string | null;
}
export interface Targeting {
  rules: Rule[];
}
export interface Segment {
  key: string;
  name: string;
  conditions: Condition[];
}
/** Who is asking: the id (OpenFeature targetingKey), properties, cohort ids. */
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

export interface Flag {
  /** For the read request's own context (an anonymous person when none was given). */
  enabled: boolean;
  value: string | null;
  /** A targeted flag's switch, base value and rules, for local evaluation. */
  targeting?: { on: boolean; value: string | null; rules: Rule[] };
}
export type Flags = Record<string, Flag>;

export interface KeyValueStorage {
  getItem(key: string): string | null | Promise<string | null>;
  setItem(key: string, value: string): unknown;
}

export interface FlagsClientOptions {
  /** The environment's public flags key (ry_flags_...). */
  key: string;
  /** The read endpoint; defaults to Railyard's. */
  url?: string;
  /** How often to read (default 30 000 ms, at least 5 000 ms). */
  pollMs?: number;
  fetch?: typeof fetch;
  /** Keeps the last good flags for an offline start (localStorage, AsyncStorage). */
  storage?: KeyValueStorage | null;
  setTimeout?: typeof setTimeout;
  clearTimeout?: typeof clearTimeout;
}

export interface FlagsClient {
  ready(): Promise<boolean>;
  refresh(): Promise<boolean>;
  isEnabled(key: string, fallback?: boolean): boolean;
  getValue(key: string, fallback?: string | null): string | null;
  has(key: string): boolean;
  all(): Flags;
  /** The flag for one person, with the server's rules and hash; null when the flag does not exist. */
  evaluate(key: string, ctx?: TargetingContext): Evaluation | null;
  segments(): Record<string, { conditions: Condition[] }>;
  source(): "default" | "cache" | "live";
  subscribe(listener: (flags: Flags) => void): () => void;
  close(): void;
}

export declare const DEFAULT_URL: string;
export declare const DEFAULT_POLL_MS: number;
export declare const MIN_POLL_MS: number;
export declare function createFlagsClient(options: FlagsClientOptions): FlagsClient;

export interface ResolutionDetails<T> {
  value: T;
  reason: string;
  variant?: string;
  errorCode?: string;
}

export declare class RailyardFlagsProvider {
  constructor(options: FlagsClientOptions & { events?: { emit(event: string, details?: unknown): void } });
  readonly metadata: { name: string };
  readonly runsOn: "client";
  initialize(): Promise<void>;
  onClose(): Promise<void>;
  resolveBooleanEvaluation(key: string, fallback: boolean, context?: Record<string, unknown>): ResolutionDetails<boolean>;
  resolveStringEvaluation(key: string, fallback: string, context?: Record<string, unknown>): ResolutionDetails<string>;
  resolveNumberEvaluation(key: string, fallback: number, context?: Record<string, unknown>): ResolutionDetails<number>;
  resolveObjectEvaluation<T>(key: string, fallback: T, context?: Record<string, unknown>): ResolutionDetails<T>;
}

export declare class RailyardServerFlagsProvider {
  constructor(options: { key: string; url?: string; ttlMs?: number; fetch?: typeof fetch; now?: () => number });
  readonly metadata: { name: string };
  readonly runsOn: "server";
  initialize(): Promise<void>;
  resolveBooleanEvaluation(key: string, fallback: boolean, context?: Record<string, unknown>): Promise<ResolutionDetails<boolean>>;
  resolveStringEvaluation(key: string, fallback: string, context?: Record<string, unknown>): Promise<ResolutionDetails<string>>;
  resolveNumberEvaluation(key: string, fallback: number, context?: Record<string, unknown>): Promise<ResolutionDetails<number>>;
  resolveObjectEvaluation<T>(key: string, fallback: T, context?: Record<string, unknown>): Promise<ResolutionDetails<T>>;
}

export declare function targetingContext(context?: Record<string, unknown>): TargetingContext;
export declare const OPERATORS: readonly Operator[];
export declare function flagHash(key: string, id: string, salt?: string): number;
export declare function matchProperty(condition: Condition & { type: "property" }, properties: Record<string, unknown> | undefined): boolean;
export declare function evaluateFlag(key: string, flag: { enabled: boolean; value: string | null; targeting?: Targeting | null }, ctx?: TargetingContext, segments?: Record<string, { conditions: Condition[] }>): Evaluation;
export declare function evaluateAnswer(key: string, flag: Flag, ctx?: TargetingContext, segments?: Record<string, { conditions: Condition[] }>): Evaluation;
export declare function parseTargeting(raw: unknown, segments?: Set<string>): Targeting | null | string;
export declare function parseSegment(raw: unknown): Segment | string;
