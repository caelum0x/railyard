export type Properties = Record<string, unknown>;

export interface ServerAnalyticsOptions {
  /** Default: ANALYTICS_SITE or NEXT_PUBLIC_ANALYTICS_SITE. */
  site?: string;
  /** Default: PRODUCT_ANALYTICS_URL or ANALYTICS_HOST, then DEFAULT_HOST. */
  host?: string;
  flushAt?: number;
  flushInterval?: number;
  env?: Record<string, string | undefined>;
  /** Passed to the underlying SDK after the defaults; for advanced use and tests. */
  sdkOptions?: Record<string, unknown>;
}

export interface ServerAnalytics {
  readonly site: string;
  readonly host: string;
  capture(distinctId: string, event: string, properties?: Properties, groups?: Record<string, string>): void;
  identify(distinctId: string, set?: Properties, setOnce?: Properties): void;
  alias(distinctId: string, alias: string): void;
  groupIdentify(groupType: string, groupKey: string, properties?: Properties): void;
  isEnabled(flag: string, distinctId: string): Promise<boolean>;
  flagValue(flag: string, distinctId: string): Promise<unknown>;
  flush(): Promise<void>;
  shutdown(): Promise<void>;
  readonly client: unknown;
}

export declare const DEFAULT_HOST: string;
export declare function readAnalyticsEnv(env?: Record<string, string | undefined>): { site?: string; host?: string };
export declare function createServerAnalytics(options?: ServerAnalyticsOptions): ServerAnalytics;
