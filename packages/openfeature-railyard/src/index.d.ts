export interface Flag {
  enabled: boolean;
  value: string | null;
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
  resolveBooleanEvaluation(key: string, fallback: boolean): ResolutionDetails<boolean>;
  resolveStringEvaluation(key: string, fallback: string): ResolutionDetails<string>;
  resolveNumberEvaluation(key: string, fallback: number): ResolutionDetails<number>;
  resolveObjectEvaluation<T>(key: string, fallback: T): ResolutionDetails<T>;
}

export declare class RailyardServerFlagsProvider {
  constructor(options: { key: string; url?: string; ttlMs?: number; fetch?: typeof fetch; now?: () => number });
  readonly metadata: { name: string };
  readonly runsOn: "server";
  initialize(): Promise<void>;
  resolveBooleanEvaluation(key: string, fallback: boolean): Promise<ResolutionDetails<boolean>>;
  resolveStringEvaluation(key: string, fallback: string): Promise<ResolutionDetails<string>>;
  resolveNumberEvaluation(key: string, fallback: number): Promise<ResolutionDetails<number>>;
  resolveObjectEvaluation<T>(key: string, fallback: T): Promise<ResolutionDetails<T>>;
}
