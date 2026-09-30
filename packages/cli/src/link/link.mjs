// `railyard link` (Wave P PY3.1, ADR RY-0004): deploy this GitHub repository
// on every push. It opens one page; there the user installs the Railyard
// GitHub App on the repository and grants Cloudflare access on Cloudflare's own
// screen (either step is skipped when it was done before). Railyard then opens
// one pull request that adds the workflow. No token is typed, pasted or shown.

import { basename, resolve } from "node:path";

import { consoleApi } from "../console-api.mjs";
import { loadCredentials } from "../credentials.mjs";
import { repoOf } from "./github.mjs";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
export const LINK_POLL_MS = 2000;
export const LINK_WAIT_MS = 15 * 60 * 1000;

export async function link(ctx, opts) {
  const creds = loadCredentials(ctx.env);
  if (!creds) {
    ctx.err("Run railyard login first.");
    return 1;
  }
  const dir = resolve(opts.dir ?? ctx.cwd);
  const repo = repoOf(dir);
  if (!repo) {
    ctx.err("This folder has no GitHub remote named origin. Push it to GitHub first, then run railyard link.");
    return 2;
  }
  const app = opts.app || basename(dir);
  const api = consoleApi({ console: creds.console, token: creds.token, fetchImpl: ctx.fetchImpl });
  const started = await api.startLink(repo, app);
  ctx.out(`Open ${started.url}`);
  ctx.out(`There: install the Railyard GitHub app on ${repo}, then allow Cloudflare access. Both are skipped if done before.`);
  if (ctx.openUrl) await ctx.openUrl(started.url).catch(() => {});
  const deadline = ctx.now() + LINK_WAIT_MS;
  while (ctx.now() < deadline) {
    const s = await api.linkStatus(started.id);
    if (s.state === "linked") {
      ctx.out(
        s.pr_url
          ? `Linked ${repo} to ${s.app}. Merge ${s.pr_url}; after that a push to ${s.production_branch} deploys, and every other branch gets a preview on its pull request.`
          : `Linked ${repo} to ${s.app}. Push to ${s.production_branch} to deploy.`,
      );
      return 0;
    }
    if (s.state === "failed") {
      ctx.err(`Not linked: ${s.message ?? "the browser step did not finish"}`);
      return 1;
    }
    await sleep(LINK_POLL_MS * (ctx.pollScale ?? 1));
  }
  ctx.err("The link page was not finished in 15 minutes. Run railyard link again.");
  return 1;
}
