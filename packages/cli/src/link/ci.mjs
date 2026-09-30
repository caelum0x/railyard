// `railyard ci` (Wave P PY3.1): the one step of the workflow `railyard link`
// adds. It runs only inside GitHub Actions with `permissions: id-token: write`:
// GitHub's OIDC token (audience = the console) proves which repository and
// branch this is; the console answers with the app, the branch and a
// short-lived Cloudflare token, which is masked in the log before it is used.
// The result goes back with a fresh OIDC token; a preview lands on the PR.

import { consoleApi } from "../console-api.mjs";
import { DEFAULT_CONSOLE, deploy } from "../commands.mjs";

async function oidcToken(ctx, audience) {
  const url = ctx.env.ACTIONS_ID_TOKEN_REQUEST_URL;
  const bearer = ctx.env.ACTIONS_ID_TOKEN_REQUEST_TOKEN;
  if (!url || !bearer) return null;
  const res = await ctx.fetchImpl(`${url}${url.includes("?") ? "&" : "?"}audience=${encodeURIComponent(audience)}`, { headers: { authorization: `Bearer ${bearer}` } });
  const body = await res.json().catch(() => ({}));
  if (!res.ok || typeof body.value !== "string") throw new Error(`GitHub gave no identity token (${res.status})`);
  return body.value;
}

export async function ci(ctx, opts) {
  if (ctx.env.GITHUB_ACTIONS !== "true") {
    ctx.err("railyard ci runs inside GitHub Actions only. On your machine use railyard deploy.");
    return 2;
  }
  const base = (ctx.env.RAILYARD_CONSOLE_URL || DEFAULT_CONSOLE).replace(/\/$/, "");
  const first = await oidcToken(ctx, base);
  if (!first) {
    ctx.err("This job cannot ask GitHub for an identity token. The workflow needs `permissions: id-token: write` (railyard link adds it).");
    return 2;
  }
  const session = await consoleApi({ console: base, token: first, fetchImpl: ctx.fetchImpl }).ciSession();
  const cfToken = session.cloudflare.access_token;
  // GitHub hides a masked value everywhere in the log from here on.
  ctx.out(`::add-mask::${cfToken}`);
  const env = { ...ctx.env, CLOUDFLARE_API_TOKEN: cfToken, CLOUDFLARE_ACCOUNT_ID: session.cloudflare.account_id };
  ctx.out(`Deploying ${session.app} from ${session.branch}${session.production ? "" : " (preview)"}`);
  let reported = null;
  const code = await deploy(
    { ...ctx, env },
    { ...opts, app: session.app, branch: session.branch },
    {
      creds: { console: base, token: null },
      noLogin: true,
      secrets: [cfToken, first],
      record: async (r) => {
        const fresh = await oidcToken(ctx, base);
        reported = await consoleApi({ console: base, token: fresh, fetchImpl: ctx.fetchImpl }).ciReport({
          status: r.status,
          url: r.url,
          framework: r.framework,
          version_id: r.version_id,
          duration_ms: r.duration_ms,
        });
      },
    },
  );
  if (reported?.comment_url) ctx.out(`Preview posted: ${reported.comment_url}`);
  return code;
}
