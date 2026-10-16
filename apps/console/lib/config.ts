// Server-only configuration. Nothing here is imported by a client component
// (test/bundle-secrets.test.ts enforces it). Names are listed once so the
// build-time secret check and the docs agree.

export const SECRET_ENV = [
  "DODO_API_KEY",
  "DODO_WEBHOOK_SECRET",
  "SCRIBASE_SERVICE_ROLE_KEY",
  "SCRIBASE_ANON_KEY",
  "RAILYARD_SESSION_SECRET",
  "RAILYARD_SCRIBASE_BRIDGE_SECRET",
  "INSIGHTS_API_TOKEN",
  "PRODUCT_ANALYTICS_CONSOLE_SECRET",
  "RELAY_TENANT_KEY",
  "RAILYARD_CREDENTIALS_KEY",
  "GITHUB_APP_PRIVATE_KEY",
  "GITHUB_APP_WEBHOOK_SECRET",
  "GITHUB_APP_CLIENT_SECRET",
  "CLOUDFLARE_OAUTH_CLIENT_SECRET",
] as const;

export const DODO_TEST_BASE = "https://test.dodopayments.com";
export const DODO_LIVE_BASE = "https://live.dodopayments.com";

/**
 * The shared Scribase control-plane (management) API -- creates/lists
 * organizations, projects and environments. Never a project's own data-plane
 * URL (the host a project's auth/rest/storage calls go to, which looks like
 * https://<host_label>.scribase.com per the box's gateway template). Shipped
 * live as SCRIBASE_URL on 2026-10-14 by mistake: it renders a 200 but every
 * sign-in silently fails (404 route_not_found on /auth/v1/otp).
 */
export const SCRIBASE_CONTROL_PLANE_HOST = "https://api.scribase.com";

export interface Config {
  consoleUrl: string;
  insightsUrl: string;
  /**
   * Railyard's code-hosting service (product name "Rithub", served off
   * apps/gitea — go-gitea/gitea, MIT). Always shown in the Nav as an
   * external link; the service itself runs native Gitea accounts until a
   * Railyard identity/OIDC source exists (TODO(railyard-sso) in
   * apps/gitea/docker-compose.yml). Override with RAILYARD_GIT_URL once the
   * real hostname is live.
   */
  gitUrl: string;
  scribaseUrl: string;
  scribaseAnonKey: string;
  scribaseServiceRoleKey: string;
  sessionSecret: string;
  /**
   * The direct-signup money ladder ($1 card check, then the plan) runs only when
   * the Dodo key, webhook secret and three product ids are all set. Without
   * them the console still runs: sign-in, Scribase add-on orgs (the bridge) and
   * every slot work, and checkout answers "card payments open soon".
   */
  billingReady: boolean;
  dodoApiKey: string;
  dodoWebhookSecret: string;
  dodoBase: string;
  dodoAllowLive: boolean;
  dodoBrandId: string | null;
  products: { cardCheck: string; month: string; year: string };
  /** Standard Webhooks secret shared with Scribase control-api (PY1.0); null keeps the bridge off. */
  scribaseBridgeSecret: string | null;
  /**
   * P-Y1 transition only: also accept bridge calls from a Scribase build that
   * does not bind the route into the signed id and sends no `addon.as_of`.
   * Off unless RAILYARD_BRIDGE_ACCEPT_LEGACY=1.
   */
  bridgeAcceptLegacy: boolean;
  /**
   * Insights depth (PY3.2: profiles, cohorts, retention by property, breakdowns,
   * export, delete by user) and web vitals (PY3.3) are served only with
   * RAILYARD_REPLAY_LIVE=1 (denylist rule railyard-slot-replay). Off: 503 "arriving".
   */
  replayLive: boolean;
  /** Insights tenant API (PY1.3); null keeps analytics proofs and pages off. */
  insightsApi: { url: string; token: string } | null;
  /**
   * Railyard Watch is shown and run only with RAILYARD_WATCH_LIVE=1 (denylist
   * rule railyard-slot-watch): set at deploy once the customer checks stack
   * passes its drill. Off: no checks run, no page, no email.
   */
  watchLive: boolean;
  /** Railyard Watch (PY2.2): the box 1 agent's bearer token; null keeps the agent API off. */
  watchAgentToken: string | null;
  /** Rally Send for alert email (Wave E contract 5.2); null logs alerts instead. */
  rallySend: { url: string; key: string } | null;
  /** Railyard Flags backend (PY2.1): the Flagsmith server; null uses Railyard's own tables. */
  flags: { apiUrl: string; adminToken: string } | null;
  /** The AI gateway (PY1.4): Relay's tenant key and the sealing key; null keeps the gateway off (503). */
  aiGateway: { relayUrl: string; relayKey: string; credentialsKey: string } | null;
  /**
   * Ship on push (PY3.1, ADR RY-0004): the Railyard GitHub App (owner item Y4)
   * and the Cloudflare OAuth client. Null until all of them and
   * RAILYARD_CREDENTIALS_KEY are set and RAILYARD_SHIP_ON_PUSH_LIVE=1; the routes then answer 503.
   */
  ship: {
    github: { appId: string; slug: string; privateKey: string; webhookSecret: string; clientId: string; clientSecret: string };
    cloudflare: { clientId: string; clientSecret: string; scopes: string[] | null };
    credentialsKey: string;
  } | null;
}

type Env = Record<string, string | undefined>;

function opt(env: Env, name: string): string {
  return env[name]?.trim() ?? "";
}

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
  const scribaseUrl = need(env, "SCRIBASE_URL").trim().replace(/\/$/, "");
  if (scribaseUrl === SCRIBASE_CONTROL_PLANE_HOST) {
    throw new Error(
      `SCRIBASE_URL is the Scribase control-plane API (${SCRIBASE_CONTROL_PLANE_HOST}), not this console's own project gateway host; ` +
        "set it to the railyard-console project's api_url once that project is provisioned (see DEPLOY.md)",
    );
  }
  return {
    consoleUrl: (env.RAILYARD_CONSOLE_URL ?? "https://railyard.ereboslab.com").replace(/\/$/, ""),
    insightsUrl: (env.RAILYARD_INSIGHTS_URL ?? "https://insights.railyard.ereboslab.com").replace(/\/$/, ""),
    gitUrl: (env.RAILYARD_GIT_URL ?? "https://git.railyard.ereboslab.com").replace(/\/$/, ""),
    scribaseUrl,
    scribaseAnonKey: need(env, "SCRIBASE_ANON_KEY"),
    scribaseServiceRoleKey: need(env, "SCRIBASE_SERVICE_ROLE_KEY"),
    sessionSecret: need(env, "RAILYARD_SESSION_SECRET"),
    billingReady: ["DODO_API_KEY", "DODO_WEBHOOK_SECRET", "DODO_PRODUCT_ID_CARD_CHECK", "DODO_PRODUCT_ID_MONTH", "DODO_PRODUCT_ID_YEAR"].every((n) => !!opt(env, n)),
    dodoApiKey: opt(env, "DODO_API_KEY"),
    dodoWebhookSecret: opt(env, "DODO_WEBHOOK_SECRET"),
    dodoBase,
    dodoAllowLive,
    dodoBrandId: env.DODO_BRAND_ID?.trim() || null,
    products: {
      cardCheck: opt(env, "DODO_PRODUCT_ID_CARD_CHECK"),
      month: opt(env, "DODO_PRODUCT_ID_MONTH"),
      year: opt(env, "DODO_PRODUCT_ID_YEAR"),
    },
    scribaseBridgeSecret: env.RAILYARD_SCRIBASE_BRIDGE_SECRET?.trim() || null,
    bridgeAcceptLegacy: env.RAILYARD_BRIDGE_ACCEPT_LEGACY === "1",
    insightsApi:
      env.INSIGHTS_API_URL?.trim() && env.INSIGHTS_API_TOKEN?.trim()
        ? { url: env.INSIGHTS_API_URL.trim().replace(/\/$/, ""), token: env.INSIGHTS_API_TOKEN.trim() }
        : null,
    watchLive: env.RAILYARD_WATCH_LIVE === "1",
    replayLive: env.RAILYARD_REPLAY_LIVE === "1",
    watchAgentToken: env.WATCH_AGENT_TOKEN?.trim() && env.WATCH_AGENT_TOKEN.trim().length >= 32 ? env.WATCH_AGENT_TOKEN.trim() : null,
    rallySend:
      env.RALLY_TX_URL?.trim().startsWith("https://") && env.RALLY_TX_KEY?.trim()
        ? { url: env.RALLY_TX_URL.trim(), key: env.RALLY_TX_KEY.trim() }
        : null,
    flags:
      env.FLAGS_API_URL?.trim() && env.FLAGS_ADMIN_TOKEN?.trim()
        ? { apiUrl: env.FLAGS_API_URL.trim().replace(/\/$/, ""), adminToken: env.FLAGS_ADMIN_TOKEN.trim() }
        : null,
    aiGateway:
      env.RELAY_URL?.trim() && env.RELAY_TENANT_KEY?.trim() && env.RAILYARD_CREDENTIALS_KEY?.trim()
        ? { relayUrl: env.RELAY_URL.trim(), relayKey: env.RELAY_TENANT_KEY.trim(), credentialsKey: env.RAILYARD_CREDENTIALS_KEY.trim() }
        : null,
    ship: readShip(env),
  };
}

function readShip(env: Env): Config["ship"] {
  const v = (name: string) => env[name]?.trim() || null;
  const appId = v("GITHUB_APP_ID");
  const slug = v("GITHUB_APP_SLUG");
  const privateKey = v("GITHUB_APP_PRIVATE_KEY");
  const webhookSecret = v("GITHUB_APP_WEBHOOK_SECRET");
  // The App's sign-in client: the link checks the person can push to the repository.
  const ghClientId = v("GITHUB_APP_CLIENT_ID");
  const ghClientSecret = v("GITHUB_APP_CLIENT_SECRET");
  const clientId = v("CLOUDFLARE_OAUTH_CLIENT_ID");
  const clientSecret = v("CLOUDFLARE_OAUTH_CLIENT_SECRET");
  const credentialsKey = v("RAILYARD_CREDENTIALS_KEY");
  // Copy and routes stay "arriving" until the owner switches it on (denylist rule railyard-slot-ship-on-push).
  if (env.RAILYARD_SHIP_ON_PUSH_LIVE !== "1") return null;
  if (!appId || !slug || !privateKey || !webhookSecret || !ghClientId || !ghClientSecret || !clientId || !clientSecret || !credentialsKey) return null;
  if (!/^\d+$/.test(appId) || !/^[a-z0-9][a-z0-9-]{0,33}$/.test(slug)) throw new Error("GITHUB_APP_ID must be a number and GITHUB_APP_SLUG the app's URL name");
  const scopes = v("CLOUDFLARE_OAUTH_SCOPES")?.split(/[\s,]+/).filter(Boolean) ?? null;
  return { github: { appId, slug, privateKey, webhookSecret, clientId: ghClientId, clientSecret: ghClientSecret }, cloudflare: { clientId, clientSecret, scopes }, credentialsKey };
}
