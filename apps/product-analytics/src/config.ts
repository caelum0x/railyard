// Service configuration from the environment. Secrets are never logged.

export interface Config {
  port: number;
  databaseUrl: string;
  /** HS256 secret the console signs its 5-minute org tokens with. */
  consoleSecret: string | null;
  /** Standard Webhooks secret shared with the console bridge (same name as the console's). */
  bridgeSecret: string | null;
  /** Site directory: the console project's REST API (read only). */
  directoryUrl: string | null;
  directoryKey: string | null;
  /** Base URL of the console, whose public flags client API is read. */
  flagsUrl: string | null;
  ragoUrl: string | null;
  ragoApiKey: string | null;
  /** "1" sends usage to the meter; anything else keeps the service's own counts only. */
  live: boolean;
}

type Env = Record<string, string | undefined>;

const opt = (env: Env, name: string): string | null => env[name]?.trim() || null;

export function readConfig(env: Env = process.env): Config {
  const databaseUrl = opt(env, "DATABASE_URL");
  if (!databaseUrl) throw new Error("DATABASE_URL is required");
  const port = Number(env.PORT ?? "8790");
  if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error("PORT must be a port number");
  const consoleSecret = opt(env, "PRODUCT_ANALYTICS_CONSOLE_SECRET");
  if (consoleSecret && consoleSecret.length < 32) throw new Error("PRODUCT_ANALYTICS_CONSOLE_SECRET must be at least 32 characters");
  return {
    port,
    databaseUrl,
    consoleSecret,
    bridgeSecret: opt(env, "RAILYARD_SCRIBASE_BRIDGE_SECRET"),
    directoryUrl: opt(env, "SITE_DIRECTORY_URL"),
    directoryKey: opt(env, "SITE_DIRECTORY_SERVICE_KEY"),
    flagsUrl: opt(env, "FLAGS_URL"),
    ragoUrl: opt(env, "RAGO_URL"),
    ragoApiKey: opt(env, "RAGO_API_KEY"),
    live: env.RAILYARD_LIVE === "1",
  };
}
