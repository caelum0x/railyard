// railyard domains [add|remove HOSTNAME] [--app NAME] [--branch B] (Wave Q QY1.5)
//
// The console attaches the hostname to the app's production Worker on your
// own Cloudflare account with your organization's Cloudflare grant; the
// token stays in the console. Without a grant there, the same calls run here
// with your local Cloudflare login (CLOUDFLARE_API_TOKEN, or the wrangler
// login), and the console only records the outcome. No token is printed.

import { readFileSync, existsSync } from "node:fs";
import { homedir, platform } from "node:os";
import { basename, join, resolve } from "node:path";

import { consoleApi } from "./console-api.mjs";
import { loadCredentials, redact } from "./credentials.mjs";
import { workerName } from "./detect.mjs";
import { addDomain, cleanHostname, cloudflareDomains, CF_API, DomainError, listDomains, removeDomain } from "./domains.mjs";
import { run, tool } from "./run.mjs";

const STATE = { active: "active (certificate issued)", pending: "pending (Cloudflare is issuing the certificate)", error: "needs attention" };

/** Where wrangler keeps its login on this machine (RAILYARD_WRANGLER_CONFIG overrides, for tests). */
export function wranglerConfigPaths(env = process.env) {
  if (env.RAILYARD_WRANGLER_CONFIG) return [env.RAILYARD_WRANGLER_CONFIG];
  const home = env.HOME || homedir();
  const xdg = env.XDG_CONFIG_HOME || join(home, ".config");
  const mac = join(home, "Library", "Preferences", ".wrangler", "config", "default.toml");
  return [...(platform() === "darwin" ? [mac] : []), join(xdg, ".wrangler", "config", "default.toml"), join(home, ".wrangler", "config", "default.toml")];
}

/** The local login's token: CLOUDFLARE_API_TOKEN, else wrangler's OAuth token (after `wrangler whoami` refreshed it). */
export async function localToken(ctx, cwd) {
  if (ctx.env.CLOUDFLARE_API_TOKEN) return ctx.env.CLOUDFLARE_API_TOKEN;
  const w = tool(ctx.env, "wrangler");
  await run(w[0], [...w.slice(1), "whoami"], { cwd, env: ctx.env });
  for (const p of wranglerConfigPaths(ctx.env)) {
    if (!existsSync(p)) continue;
    const m = /^\s*oauth_token\s*=\s*"([^"]+)"/m.exec(readFileSync(p, "utf8"));
    if (m) return m[1];
  }
  return null;
}

async function localAccount(ctx, token) {
  if (ctx.env.CLOUDFLARE_ACCOUNT_ID) return ctx.env.CLOUDFLARE_ACCOUNT_ID;
  const base = (ctx.env.RAILYARD_CF_API || CF_API).replace(/\/$/, "");
  const res = await ctx.fetchImpl(`${base}/accounts?per_page=50`, { headers: { authorization: `Bearer ${token}` } });
  const body = await res.json().catch(() => ({}));
  if (!res.ok) throw new DomainError(`Cloudflare refused this login (${res.status}). Log in again with wrangler login, or set CLOUDFLARE_API_TOKEN.`, 403);
  const list = Array.isArray(body.result) ? body.result : [];
  if (list.length === 1) return list[0].id;
  throw new DomainError(list.length ? "This login reaches several Cloudflare accounts. Set CLOUDFLARE_ACCOUNT_ID to the one the app is deployed to." : "No Cloudflare account was found for this login.", 400);
}

async function local(ctx, dir) {
  const token = await localToken(ctx, dir);
  if (!token) throw new DomainError("Log in to Cloudflare first (wrangler login), then run this again.", 401);
  const accountId = await localAccount(ctx, token);
  return { token, cf: cloudflareDomains({ token, accountId, fetchImpl: ctx.fetchImpl, apiBase: ctx.env.RAILYARD_CF_API }) };
}

export async function domains(ctx, sub, hostArg, opts) {
  const creds = loadCredentials(ctx.env);
  if (!creds) {
    ctx.err("Run railyard login first.");
    return 1;
  }
  const dir = resolve(opts.dir ?? ctx.cwd);
  const app = (opts.app || basename(dir)).toLowerCase().replace(/[^a-z0-9-]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40);
  const api = consoleApi({ console: creds.console, token: creds.token, fetchImpl: ctx.fetchImpl });
  const secrets = [creds.token];
  const say = (s) => ctx.out(redact(s, secrets));
  try {
    if (!sub || sub === "list") {
      const { domains: list } = await api.domains(app);
      if (!list?.length) say(`No custom domains on ${app} yet. Add one: railyard domains add app.example.com`);
      for (const d of list ?? []) say(`${d.hostname}  ${STATE[d.state] ?? d.state}`);
      return 0;
    }
    const hostname = cleanHostname(hostArg);
    if (!hostname) {
      ctx.err("Give a full hostname, for example: railyard domains add app.example.com");
      return 2;
    }
    if (sub !== "add" && sub !== "remove") {
      ctx.err(`Unknown domains command ${sub}. Use add, remove or nothing (to list).`);
      return 2;
    }
    const r = await api.domainChange(sub === "add" ? "POST" : "DELETE", app, hostname);
    if (r.status === 201 || r.status === 200) {
      if (sub === "add") say(`Attached ${hostname} to ${r.body.domain.worker}: ${STATE[r.body.domain.state] ?? r.body.domain.state}.`);
      else say(r.body.removed ? `Removed ${hostname}.` : `${hostname} was not attached; nothing to remove.`);
      return 0;
    }
    if (r.status !== 409 || r.body.code !== "no_cloudflare_grant") {
      ctx.err(r.body.error || `Railyard answered ${r.status}`);
      return 1;
    }
    // No grant in the console: the same calls with the local login; the console records the outcome.
    const { token, cf } = await local(ctx, dir);
    secrets.push(token);
    if (sub === "add") {
      const worker = workerName(app, opts.branch || "main");
      const d = await addDomain(cf, hostname, worker);
      await api.recordDomain({ app, hostname, state: d.state, cf_domain_id: d.id, zone_name: d.zone_name });
      say(`Attached ${hostname} to ${worker} with your Cloudflare login: ${STATE[d.state] ?? d.state}.`);
      return 0;
    }
    const removed = await removeDomain(cf, hostname);
    await api.recordDomain({ app, hostname, removed: true });
    say(removed.removed ? `Removed ${hostname}.` : `${hostname} was not attached; nothing to remove.`);
    return 0;
  } catch (e) {
    ctx.err(redact(e instanceof DomainError || e.status ? e.message : `Railyard could not finish: ${e.message}`, secrets));
    return 1;
  }
}

export { listDomains };
