// railyard login | deploy | rollback | deploys

import { existsSync } from "node:fs";
import { basename, join, resolve } from "node:path";

import { consoleApi } from "./console-api.mjs";
import { loadCredentials, redact, saveCredentials } from "./credentials.mjs";
import { detect, workerName } from "./detect.mjs";
import { COMPAT_DATE, git, parseDeployOutput, run, tool } from "./run.mjs";
import { projectEnv, wranglerVars } from "./scribase/env.mjs";

export const DEFAULT_CONSOLE = "https://railyard.ereboslab.com";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

export async function login(ctx) {
  const base = ctx.env.RAILYARD_CONSOLE_URL || DEFAULT_CONSOLE;
  const api = consoleApi({ console: base, fetchImpl: ctx.fetchImpl });
  const start = await api.deviceCode();
  ctx.out(`Open ${start.verification_uri} and enter ${start.user_code}`);
  ctx.out(`(or open ${start.verification_uri_complete})`);
  const deadline = ctx.now() + start.expires_in * 1000;
  while (ctx.now() < deadline) {
    const r = await api.deviceToken(start.device_code);
    if (r.status === 200 && r.body.access_token) {
      const path = saveCredentials({ console: base, token: r.body.access_token, org: r.body.org_id }, ctx.env);
      ctx.out(`Signed in. Saved to ${path} (only you can read it).`);
      return 0;
    }
    if (r.body.error && r.body.error !== "authorization_pending") {
      ctx.err(`Sign-in stopped: ${r.body.error === "expired_token" ? "the code expired" : "the code was not accepted"}. Run railyard login again.`);
      return 1;
    }
    await sleep((start.interval ?? 3) * 1000 * (ctx.pollScale ?? 1));
  }
  ctx.err("Sign-in timed out. Run railyard login again.");
  return 1;
}

function needLogin(ctx) {
  const creds = loadCredentials(ctx.env);
  if (!creds) ctx.err("Run railyard login first.");
  return creds;
}

async function ensureWrangler(ctx, cwd, noLogin = false) {
  const w = tool(ctx.env, "wrangler");
  const who = await run(w[0], [...w.slice(1), "whoami"], { cwd, env: ctx.env });
  if (who.code === 0 && !/not authenticated/i.test(who.stdout + who.stderr)) return true;
  // In CI there is no browser: a refused token is an error, never a login prompt.
  if (noLogin) return false;
  ctx.out("Log in to Cloudflare in your browser (wrangler login). Railyard never sees this login.");
  const login = await run(w[0], [...w.slice(1), "login"], { cwd, env: ctx.env, inherit: true });
  return login.code === 0;
}

/**
 * `hooks` (used by `railyard ci`, PY3.1): `creds` instead of the saved login, `noLogin`,
 * `record` instead of the console deploy record, `secrets` to redact as well.
 */
export async function deploy(ctx, opts, hooks = {}) {
  const creds = hooks.creds ?? needLogin(ctx);
  if (!creds) return 1;
  const dir = resolve(opts.dir ?? ctx.cwd);
  const found = detect(dir);
  if (!found.ok) {
    ctx.err(found.message);
    return 2;
  }
  const app = opts.app || basename(dir);
  const branch = opts.branch || git(dir, ["rev-parse", "--abbrev-ref", "HEAD"]) || "main";
  const commit = git(dir, ["rev-parse", "HEAD"]);
  const name = workerName(app, branch);
  // A Scribase project folder: its public URL and anon key go into the build
  // and the Worker; secrets in the same file are left out (PY1.2).
  const sb = projectEnv(dir, branch);
  const secrets = [creds.token, ...(hooks.secrets ?? []), ...Object.entries(sb.values).filter(([k]) => /KEY/.test(k)).map(([, v]) => v)];
  const echo = (s) => ctx.out(redact(s, secrets).replace(/\n$/, ""));
  const started = ctx.now();
  const api = consoleApi({ console: creds.console, token: creds.token, fetchImpl: ctx.fetchImpl });

  if (!(await ensureWrangler(ctx, dir, hooks.noLogin))) {
    ctx.err(hooks.noLogin ? "Cloudflare refused the access token. Run railyard link again to renew the grant." : "Cloudflare login did not finish. Run railyard deploy again.");
    return 1;
  }
  ctx.out(`Deploying ${found.framework} app ${app} (branch ${branch}) as ${name}`);
  if (sb.found) ctx.out(`Using the Scribase ${sb.branch ? "branch" : "project"} settings: ${sb.names.join(", ")}`);
  if (sb.refused.length) ctx.out(`Left out as secrets: ${sb.refused.join(", ")}`);
  let result = { code: 1, stdout: "", stderr: "" };
  const w = tool(ctx.env, "wrangler");
  const buildEnv = { ...ctx.env, ...sb.values };
  const vars = wranglerVars(sb.values);
  if (found.framework === "nextjs") {
    const o = tool(ctx.env, "opennext");
    const b = await run(o[0], [...o.slice(1), "build"], { cwd: dir, env: buildEnv, echo });
    if (b.code === 0) {
      result = await run(
        w[0],
        [...w.slice(1), "deploy", join(".open-next", "worker.js"), "--name", name, "--assets", join(".open-next", "assets"), "--compatibility-date", COMPAT_DATE, "--compatibility-flags", "nodejs_compat", ...vars],
        { cwd: dir, env: buildEnv, echo },
      );
    } else result = b;
  } else {
    const b = found.build ? await run(found.build[0], found.build.slice(1), { cwd: dir, env: buildEnv, echo }) : { code: 0, stdout: "", stderr: "" };
    if (b.code !== 0) {
      result = b;
    } else {
      const assets = found.outDir;
      if (!existsSync(join(dir, assets))) {
        ctx.err(`The build did not produce ${assets}/. Check your build script.`);
        return 1;
      }
      result = await run(w[0], [...w.slice(1), "deploy", "--name", name, "--assets", assets, "--compatibility-date", COMPAT_DATE, ...vars], { cwd: dir, env: buildEnv, echo });
    }
  }
  const { url, versionId } = parseDeployOutput(result.stdout + result.stderr);
  const ok = result.code === 0 && !!url;
  const record = { app, url: url ?? "", commit_sha: commit, branch, framework: found.framework, version_id: versionId, duration_ms: ctx.now() - started, status: ok ? "succeeded" : "failed" };
  try {
    await (hooks.record ? hooks.record(record) : api.recordDeploy(record));
  } catch (e) {
    ctx.err(`Deployed, but the console did not record it: ${e.message}`);
  }
  if (!ok) {
    ctx.err("Deploy failed. The output above says why.");
    return 1;
  }
  ctx.out(`Live: ${url}`);
  return 0;
}

export async function rollback(ctx, opts) {
  const creds = needLogin(ctx);
  if (!creds) return 1;
  const dir = resolve(opts.dir ?? ctx.cwd);
  const app = opts.app || basename(dir);
  const branch = opts.branch || git(dir, ["rev-parse", "--abbrev-ref", "HEAD"]) || "main";
  const api = consoleApi({ console: creds.console, token: creds.token, fetchImpl: ctx.fetchImpl });
  const { deploys } = await api.listDeploys(app, branch);
  const good = (deploys ?? []).filter((d) => d.status === "succeeded" && d.version_id);
  if (good.length < 2) {
    ctx.err(`Nothing to roll back to: ${app} on ${branch} has ${good.length} successful deploy${good.length === 1 ? "" : "s"}.`);
    return 1;
  }
  const [current, previous] = good;
  const name = workerName(app, branch);
  const w = tool(ctx.env, "wrangler");
  const started = ctx.now();
  const r = await run(w[0], [...w.slice(1), "rollback", previous.version_id, "--name", name, "--message", `railyard rollback to ${previous.version_id}`, "--yes"], {
    cwd: dir,
    env: ctx.env,
    echo: (s) => ctx.out(redact(s, [creds.token]).replace(/\n$/, "")),
  });
  const ok = r.code === 0;
  await api.recordDeploy({
    app,
    url: previous.url,
    commit_sha: previous.commit_sha,
    branch,
    framework: previous.framework,
    version_id: previous.version_id,
    duration_ms: ctx.now() - started,
    status: ok ? "succeeded" : "failed",
    rollback_of: current.id,
  });
  if (!ok) {
    ctx.err("Rollback failed. The output above says why.");
    return 1;
  }
  ctx.out(`Rolled back ${name} to version ${previous.version_id}: ${previous.url}`);
  return 0;
}

const FORMATS = new Set(["pretty", "json"]);

/** railyard logs [--app NAME] [--branch B] [--format pretty|json] [--status ok|error|canceled]: stream live Worker logs (RYP-D5). */
export async function logs(ctx, opts) {
  const creds = needLogin(ctx);
  if (!creds) return 1;
  const format = opts.format || "pretty";
  if (!FORMATS.has(format)) {
    ctx.err(`Unknown --format ${format}. Use pretty or json.`);
    return 2;
  }
  const dir = resolve(opts.dir ?? ctx.cwd);
  const app = opts.app || basename(dir);
  const branch = opts.branch || git(dir, ["rev-parse", "--abbrev-ref", "HEAD"]) || "main";
  const name = workerName(app, branch);
  if (!(await ensureWrangler(ctx, dir))) {
    ctx.err("Cloudflare login did not finish. Run railyard logs again.");
    return 1;
  }
  const w = tool(ctx.env, "wrangler");
  const args = [...w.slice(1), "tail", "--name", name, "--format", format];
  if (opts.status) args.push("--status", opts.status);
  ctx.out(`Streaming ${name}. Stop with Ctrl+C.`);
  const result = await run(w[0], args, {
    cwd: dir,
    env: ctx.env,
    echo: (s) => ctx.out(redact(s, [creds.token]).replace(/\n$/, "")),
  });
  // A Ctrl+C mid-stream ends the child by signal, not a bad exit code: that is how a
  // tail is meant to stop, not a failure (run() reports it as code 0, signal set).
  if (result.code !== 0) {
    ctx.err("Could not stream logs. The output above says why.");
    return 1;
  }
  return 0;
}

export async function listDeploys(ctx, opts) {
  const creds = needLogin(ctx);
  if (!creds) return 1;
  const app = opts.app || basename(resolve(opts.dir ?? ctx.cwd));
  const api = consoleApi({ console: creds.console, token: creds.token, fetchImpl: ctx.fetchImpl });
  const { deploys } = await api.listDeploys(app, opts.branch);
  if (!deploys?.length) {
    ctx.out(`No deploys of ${app} yet. Run railyard deploy.`);
    return 0;
  }
  for (const d of deploys) ctx.out(`${d.created_at}  ${d.status.padEnd(9)}  ${d.branch.padEnd(16)}  ${d.url}`);
  return 0;
}
