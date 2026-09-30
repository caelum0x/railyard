import type { ConsentState, Properties } from "./index";

export type { ConsentState, Properties } from "./index";

export interface NativeAnalyticsOptions {
  /** Default: EXPO_PUBLIC_ANALYTICS_SITE. */
  site?: string;
  /** Default: EXPO_PUBLIC_ANALYTICS_HOST, then DEFAULT_HOST. */
  host?: string;
  /** Default true: no client exists and nothing is sent until setConsent("granted"). */
  requireConsent?: boolean;
  consent?: ConsentState;
  /** Passed to the underlying SDK after the defaults; for advanced use and tests. */
  sdkOptions?: Record<string, unknown>;
}

export interface NativeAnalytics {
  readonly site: string;
  readonly host: string;
  readonly consent: ConsentState;
  setConsent(state: ConsentState): void;
  capture(event: string, properties?: Properties): void;
  screen(name: string, properties?: Properties): void;
  identify(distinctId: string, properties?: Properties): void;
  alias(alias: string): void;
  group(type: string, key: string, properties?: Properties): void;
  reset(): void;
  flush(): Promise<void>;
  shutdown(): Promise<void>;
  isEnabled(flag: string): boolean;
  readonly client: unknown | null;
}

export declare const DEFAULT_HOST: string;
export declare const NATIVE_PRIVACY_DEFAULTS: Readonly<Record<string, unknown>>;
export declare function readAnalyticsEnv(): { site?: string; host?: string };
export declare function createNativeAnalytics(options?: NativeAnalyticsOptions): NativeAnalytics;
export declare function screenTracker(analytics: Pick<NativeAnalytics, "screen">): (route: string, properties?: Properties) => boolean;
