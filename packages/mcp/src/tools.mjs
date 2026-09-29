// Railyard MCP tools (RYP-D5). Every tool declares readOnlyHint or
// destructiveHint (test/annotations.test.mjs). Deploys run on the user's
// machine, so deploy.run returns the exact command instead of deploying.

const NAME = { type: "string", pattern: "^[a-z0-9][a-z0-9-]{0,39}$", description: "App name (letters, numbers, dashes)" };
const BRANCH = { type: "string", maxLength: 200, description: "Git branch; main or master is production" };

const readOnly = { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false };

class ToolError extends Error {}

function needLogin(ctx) {
  if (!ctx.creds) throw new ToolError("Not signed in. Run `npx railyard login` in a terminal, then try again.");
  return ctx.creds;
}

async function consoleGet(ctx, path) {
  const creds = needLogin(ctx);
  const res = await ctx.fetchImpl(`${creds.console.replace(/\/$/, "")}${path}`, { headers: { authorization: `Bearer ${creds.token}` } });
  const body = await res.json().catch(() => ({}));
  if (res.status === 401) throw new ToolError("The Railyard sign-in on this machine expired. Run `npx railyard login`.");
  if (!res.ok) throw new ToolError(body.error ? `Railyard: ${body.error}` : `Railyard answered ${res.status}`);
  return body;
}

const shellArg = (s) => (/^[A-Za-z0-9._/-]+$/.test(s) ? s : `'${s.replace(/'/g, "'\\''")}'`);

export const TOOLS = [
  {
    name: "deploy.run",
    title: "Deploy command",
    description: "Returns the exact command that deploys this project to the user's own Cloudflare account. Deploys run on the user's machine: run the returned command in the project folder.",
    inputSchema: { type: "object", additionalProperties: false, properties: { app: NAME, branch: BRANCH } },
    annotations: readOnly,
    async handler(ctx, args) {
      const parts = ["npx", "railyard", "deploy"];
      if (args.app) parts.push("--app", shellArg(args.app));
      if (args.branch) parts.push("--branch", shellArg(args.branch));
      const command = parts.join(" ");
      return {
        signedIn: !!ctx.creds,
        steps: ctx.creds ? [command] : ["npx railyard login", command],
        command,
        note: "Run in the project folder. The first deploy opens a Cloudflare login in the browser; no key is needed.",
      };
    },
  },
  {
    name: "deploy.list",
    title: "Recent deploys",
    description: "Lists recent deploys of the signed-in org: URL, branch, commit, framework, duration and status, newest first.",
    inputSchema: { type: "object", additionalProperties: false, properties: { app: NAME, branch: BRANCH, limit: { type: "integer", minimum: 1, maximum: 100 } } },
    annotations: readOnly,
    async handler(ctx, args) {
      const q = new URLSearchParams();
      if (args.app) q.set("app", args.app);
      if (args.branch) q.set("branch", args.branch);
      const { deploys } = await consoleGet(ctx, `/api/v1/deploys?${q}`);
      return { deploys: (deploys ?? []).slice(0, args.limit ?? 20) };
    },
  },
  {
    name: "insights.summary",
    title: "Visitors and events",
    description: "Page views, visitors and top pages for an app over the last N days.",
    inputSchema: { type: "object", additionalProperties: false, required: ["app"], properties: { app: NAME, days: { type: "integer", minimum: 1, maximum: 90 } } },
    annotations: readOnly,
    async handler(ctx, args) {
      return insightsQuery(ctx, "summary", { app: args.app, days: String(args.days ?? 7) });
    },
  },
  {
    name: "insights.funnel",
    title: "Funnel",
    description: "Step-by-step conversion between events or pages for an app, for example signup then first deploy.",
    inputSchema: {
      type: "object",
      additionalProperties: false,
      required: ["app", "steps"],
      properties: { app: NAME, steps: { type: "array", minItems: 2, maxItems: 8, items: { type: "string", minLength: 1, maxLength: 200 } }, days: { type: "integer", minimum: 1, maximum: 90 } },
    },
    annotations: readOnly,
    async handler(ctx, args) {
      return insightsQuery(ctx, "funnel", { app: args.app, steps: args.steps.join("\n"), days: String(args.days ?? 7) });
    },
  },
  {
    name: "flags.list",
    title: "Flags",
    description: "Lists the org's flags. The Flags module is arriving; until it is live this says so and lists nothing.",
    inputSchema: { type: "object", additionalProperties: false, properties: { app: NAME } },
    annotations: readOnly,
    async handler(ctx) {
      needLogin(ctx);
      return { status: "arriving", flags: [], message: "The Flags module is not live yet. Nothing to list." };
    },
  },
  {
    name: "gateway.traces",
    title: "Gateway traces",
    description: "Recent AI gateway calls made with the gateway key in this environment: model, provider, tokens, latency, cost and finish reason. Only that key's own calls are visible.",
    inputSchema: { type: "object", additionalProperties: false, properties: { limit: { type: "integer", minimum: 1, maximum: 100 }, hours: { type: "integer", minimum: 1, maximum: 2160 } } },
    annotations: readOnly,
    async handler(ctx, args) {
      const url = ctx.env.RAILYARD_GATEWAY_URL;
      const key = ctx.env.RAILYARD_GATEWAY_KEY;
      if (!url || !key) return { status: "not_connected", traces: [], message: "No gateway is connected in this environment, so there are no traces to show." };
      const q = new URLSearchParams({ limit: String(args.limit ?? 20), window_hours: String(args.hours ?? 24) });
      const res = await ctx.fetchImpl(`${url.replace(/\/$/, "")}/relay/traces?${q}`, { headers: { authorization: `Bearer ${key}` } });
      if (res.status === 401 || res.status === 403) throw new ToolError("The gateway key in this environment was refused.");
      if (!res.ok) throw new ToolError(`The gateway answered ${res.status}`);
      const page = await res.json();
      return {
        traces: (page.traces ?? []).map((t) => ({
          id: t.inference_id,
          at: new Date(Math.floor(t.created_at_us / 1000)).toISOString(),
          function: t.function_name,
          model: t.model_name ?? null,
          provider: t.model_provider_name ?? null,
          input_tokens: t.input_tokens ?? null,
          output_tokens: t.output_tokens ?? null,
          cost_usd: typeof t.cost_micros === "number" ? t.cost_micros / 1e6 : null,
          ms: t.processing_time_ms ?? null,
          finish_reason: t.finish_reason ?? null,
        })),
        next_before: page.next_before ?? null,
      };
    },
  },
  {
    name: "usage.current",
    title: "Plan and usage",
    description: "The signed-in org's plan state, trial days left, app count against the plan, and deploys so far.",
    inputSchema: { type: "object", additionalProperties: false, properties: {} },
    annotations: readOnly,
    async handler(ctx) {
      const me = await consoleGet(ctx, "/api/v1/me");
      const { deploys } = await consoleGet(ctx, "/api/v1/deploys");
      return {
        org: me.org,
        plan_state: me.access?.state,
        trial_days_left: me.access?.trialDaysLeft ?? null,
        apps: { used: me.apps?.length ?? 0, limit: me.access?.appsLimit ?? 0, names: (me.apps ?? []).map((a) => a.name) },
        events_limit_per_month: me.access?.eventsLimit ?? 0,
        deploys_listed: (deploys ?? []).length,
      };
    },
  },
];

async function insightsQuery(ctx, kind, params) {
  needLogin(ctx);
  const url = ctx.env.RAILYARD_INSIGHTS_QUERY_URL;
  if (!url) {
    return { status: "no_data_yet", message: "Insights has no query store yet, so there are no numbers to report. Events sent now are counted at the edge." };
  }
  const res = await ctx.fetchImpl(`${url.replace(/\/$/, "")}/v1/query/${kind}?${new URLSearchParams(params)}`, { headers: { authorization: `Bearer ${ctx.creds.token}` } });
  if (res.status === 404) return { status: "no_data_yet", message: `No events for ${params.app} yet.` };
  if (!res.ok) throw new ToolError(`Insights answered ${res.status}`);
  return { status: "ok", ...(await res.json()) };
}

// Minimal JSON-schema check for the shapes above (types, required, bounds, patterns, no extra keys).
export function validate(schema, value, path = "arguments") {
  if (schema.type === "object") {
    if (!value || typeof value !== "object" || Array.isArray(value)) return `${path} must be an object`;
    for (const r of schema.required ?? []) if (value[r] === undefined) return `${path}.${r} is required`;
    for (const [k, v] of Object.entries(value)) {
      const s = schema.properties?.[k];
      if (!s) {
        if (schema.additionalProperties === false) return `${path}.${k} is not a known argument`;
        continue;
      }
      const e = validate(s, v, `${path}.${k}`);
      if (e) return e;
    }
    return null;
  }
  if (schema.type === "string") {
    if (typeof value !== "string") return `${path} must be a string`;
    if (schema.minLength && value.length < schema.minLength) return `${path} is too short`;
    if (schema.maxLength && value.length > schema.maxLength) return `${path} is too long`;
    if (schema.pattern && !new RegExp(schema.pattern).test(value)) return `${path} has the wrong format`;
    return null;
  }
  if (schema.type === "integer") {
    if (!Number.isInteger(value)) return `${path} must be an integer`;
    if (schema.minimum !== undefined && value < schema.minimum) return `${path} must be at least ${schema.minimum}`;
    if (schema.maximum !== undefined && value > schema.maximum) return `${path} must be at most ${schema.maximum}`;
    return null;
  }
  if (schema.type === "array") {
    if (!Array.isArray(value)) return `${path} must be a list`;
    if (schema.minItems && value.length < schema.minItems) return `${path} needs at least ${schema.minItems} items`;
    if (schema.maxItems && value.length > schema.maxItems) return `${path} takes at most ${schema.maxItems} items`;
    for (let i = 0; i < value.length; i += 1) {
      const e = validate(schema.items, value[i], `${path}[${i}]`);
      if (e) return e;
    }
    return null;
  }
  return null;
}

export { ToolError };
