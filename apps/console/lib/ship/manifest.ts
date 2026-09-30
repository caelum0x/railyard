// The Railyard GitHub App manifest (owner item Y4): the owner opens
// /api/v1/github/manifest once, confirms on GitHub, and GitHub creates the App
// with these exact permissions and URLs. The conversion (which returns the App
// key) runs on the owner's machine with the one-line command the done page
// shows, straight into `wrangler secret put`, so no key is shown on a page.

import { page } from "./http.ts";

export interface Manifest {
  name: string;
  url: string;
  hook_attributes: { url: string; active: boolean };
  redirect_url: string;
  /** Where GitHub returns after the user signs in with the App (the link's "who is this" check). */
  callback_urls: string[];
  request_oauth_on_install: boolean;
  setup_url: string;
  setup_on_update: boolean;
  public: boolean;
  default_permissions: Record<string, "read" | "write">;
  default_events: string[];
}

export function manifest(consoleUrl: string): Manifest {
  const c = consoleUrl.replace(/\/$/, "");
  return {
    name: "Railyard",
    url: c,
    hook_attributes: { url: `${c}/api/v1/github/webhook`, active: true },
    redirect_url: `${c}/api/v1/github/manifest/done`,
    callback_urls: [`${c}/api/v1/github/user/callback`],
    request_oauth_on_install: false,
    setup_url: `${c}/api/v1/github/setup`,
    setup_on_update: true,
    public: true,
    // contents + workflows: open the one pull request that adds the workflow file;
    // pull_requests: post the preview URL; metadata: required by GitHub.
    default_permissions: { contents: "write", workflows: "write", pull_requests: "write", metadata: "read" },
    default_events: ["installation_repositories"],
  };
}

const esc = (s: string) => s.replace(/[&<>"']/g, (ch) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[ch] as string);

/** A page that posts the manifest to GitHub (optionally for a GitHub organization). */
export function manifestPage(consoleUrl: string, org: string | null): Response {
  if (org !== null && !/^[A-Za-z0-9][A-Za-z0-9-]{0,38}$/.test(org)) return page("Not valid", "org must be a GitHub organization name.", 400);
  const action = org ? `https://github.com/organizations/${org}/settings/apps/new` : "https://github.com/settings/apps/new";
  const body = `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Create the Railyard GitHub App</title></head>
<body style="font:16px/1.5 system-ui,sans-serif;max-width:36rem;margin:15vh auto;padding:0 1.5rem">
<h1 style="font-size:1.25rem">Create the Railyard GitHub App</h1>
<p>GitHub will show the permissions (contents, workflows and pull requests write; metadata read) and ask you to confirm.</p>
<form method="post" action="${esc(action)}"><input type="hidden" name="manifest" value="${esc(JSON.stringify(manifest(consoleUrl)))}"><button type="submit">Continue on GitHub</button></form>
</body></html>`;
  return new Response(body, { headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" } });
}

/** After GitHub created the App: the one command that stores its keys as console secrets. */
export function manifestDone(code: string | null): Response {
  if (!code || !/^[A-Za-z0-9_-]{8,100}$/.test(code)) return page("Not valid", "GitHub sent no code. Open /api/v1/github/manifest again.", 400);
  const cmd = [
    `curl -fsS -X POST https://api.github.com/app-manifests/${code}/conversions > /tmp/railyard-app.json`,
    `jq -r .id /tmp/railyard-app.json | npx wrangler secret put GITHUB_APP_ID`,
    `jq -r .slug /tmp/railyard-app.json | npx wrangler secret put GITHUB_APP_SLUG`,
    `jq -r .pem /tmp/railyard-app.json | npx wrangler secret put GITHUB_APP_PRIVATE_KEY`,
    `jq -r .webhook_secret /tmp/railyard-app.json | npx wrangler secret put GITHUB_APP_WEBHOOK_SECRET`,
    `jq -r .client_id /tmp/railyard-app.json | npx wrangler secret put GITHUB_APP_CLIENT_ID`,
    `jq -r .client_secret /tmp/railyard-app.json | npx wrangler secret put GITHUB_APP_CLIENT_SECRET`,
    `rm -f /tmp/railyard-app.json`,
  ].join(" && \\\n  ");
  const body = `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Railyard GitHub App created</title></head>
<body style="font:16px/1.5 system-ui,sans-serif;max-width:44rem;margin:10vh auto;padding:0 1.5rem">
<h1 style="font-size:1.25rem">The Railyard GitHub App exists</h1>
<p>Within the next hour, run this once in <code>railyard/apps/console</code> on your machine. It stores the App's id, key, webhook secret and sign-in client as console secrets without printing them:</p>
<pre style="white-space:pre-wrap;background:#f1f3f6;padding:1rem">${esc(cmd)}</pre>
</body></html>`;
  return new Response(body, { headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store", "referrer-policy": "no-referrer" } });
}
