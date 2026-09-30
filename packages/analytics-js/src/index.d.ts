export type ConsentState = "pending" | "granted" | "denied";

export interface RecordingPrivacy {
  maskAllInputs?: boolean;
  maskTextSelector?: string | null;
  blockSelector?: string | null;
  recordCrossOriginIframes?: boolean;
  recordHeaders?: boolean;
  recordBody?: boolean;
  captureCanvas?: { recordCanvas?: boolean };
}

export interface AnalyticsOptions {
  /** The app's public site key. Default: NEXT_PUBLIC_ANALYTICS_SITE, EXPO_PUBLIC_ANALYTICS_SITE or VITE_ANALYTICS_SITE. */
  site?: string;
  /** Default: NEXT_PUBLIC_ANALYTICS_HOST (or EXPO_PUBLIC_ / VITE_), then DEFAULT_HOST. */
  host?: string;
  /** Default true: nothing is loaded, stored or sent until setConsent("granted"). */
  requireConsent?: boolean;
  consent?: ConsentState;
  capturePageview?: boolean | "history_change";
  capturePageleave?: boolean;
  autocapture?: boolean;
  /** Only ever loosens the recording privacy defaults on purpose. */
  recording?: RecordingPrivacy;
  name?: string;
  debug?: boolean;
  /** Passed to the underlying SDK after the defaults; for advanced use. */
  sdkOptions?: Record<string, unknown>;
}

export type Properties = Record<string, unknown>;

export interface Analytics {
  readonly site: string;
  readonly host: string;
  readonly consent: ConsentState;
  setConsent(state: ConsentState): void;
  capture(event: string, properties?: Properties): void;
  identify(distinctId: string, set?: Properties, setOnce?: Properties): void;
  alias(alias: string): void;
  group(type: string, key: string, properties?: Properties): void;
  setPersonProperties(set?: Properties, setOnce?: Properties): void;
  reset(): void;
  isEnabled(flag: string): boolean;
  flagValue(flag: string): unknown;
  onFlags(cb: (flags: string[]) => void): void;
  /** The underlying SDK client once consent is granted, else null. */
  readonly client: unknown | null;
}

export declare const DEFAULT_HOST: string;
export declare const PRIVACY_DEFAULTS: Readonly<Record<string, unknown>>;
export declare function readAnalyticsEnv(): { site?: string; host?: string };
export declare function createAnalytics(options?: AnalyticsOptions): Analytics;
