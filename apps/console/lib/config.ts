// Server-only configuration. Nothing here is imported by a client component
// (test/bundle-secrets.test.ts enforces it). Names are listed once so the
// build-time secret check and the docs agree.

export const SECRET_ENV = [
  "DODO_API_KEY",
  "DODO_WEBHOOK_SECRET",
  "SCRIBASE_SERVICE_ROLE_KEY",
  "SCRIBASE_ANON_KEY",
  "RAILYARD_SESSION_SECRET",
] as const;

export const DODO_TEST_BASE = "https://test.dodopayments.com";
export const DODO_LIVE_BASE = "https://live.dodopayments.com";

export interface Config {
  consoleUrl: string;
  insightsUrl: string;
  scribaseUrl: string;
  scribaseAnonKey: string;
  scribaseServiceRoleKey: string;
  sessionSecret: string;
  dodoApiKey: string;
  dodoWebhookSecret: string;
  dodoBase: string;
  dodoAllowLive: boolean;
  dodoBrandId: string | null;
  products: { cardCheck: string; month: string; year: string };
}

type Env = Record<string, string | undefined>;

function need(env: Env, name: string): string {
  const v = env[name];
  if (!v || !v.trim()) throw new Error(`${name} is not set`);
  return v.trim();
}

export function readConfig(env: Env = process.env): Config {
  const dodoBase = (env.DODO_API_BASE ?? DODO_TEST_BASE).replace(/\/$/, "");
  const dodoAllowLive = env.RAILYARD_DODO_LIVE === "1";
  if (dodoBase === DODO_LIVE_BASE && !dodoAllowLive) {
    throw new Error("DODO_API_BASE is the live host but RAILYARD_DODO_LIVE is not 1 (test mode only until the owner switches)");
  }
  return {
    consoleUrl: (env.RAILYARD_CONSOLE_URL ?? "https://railyard.ereboslab.com").replace(/\/$/, ""),
    insightsUrl: (env.RAILYARD_INSIGHTS_URL ?? "https://insights.railyard.ereboslab.com").replace(/\/$/, ""),
    scribaseUrl: need(env, "SCRIBASE_URL").replace(/\/$/, ""),
    scribaseAnonKey: need(env, "SCRIBASE_ANON_KEY"),
    scribaseServiceRoleKey: need(env, "SCRIBASE_SERVICE_ROLE_KEY"),
    sessionSecret: need(env, "RAILYARD_SESSION_SECRET"),
    dodoApiKey: need(env, "DODO_API_KEY"),
    dodoWebhookSecret: need(env, "DODO_WEBHOOK_SECRET"),
    dodoBase,
    dodoAllowLive,
    dodoBrandId: env.DODO_BRAND_ID?.trim() || null,
    products: {
      cardCheck: need(env, "DODO_PRODUCT_ID_CARD_CHECK"),
      month: need(env, "DODO_PRODUCT_ID_MONTH"),
      year: need(env, "DODO_PRODUCT_ID_YEAR"),
    },
  };
}
