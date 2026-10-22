// Child processes: git, the project's build, wrangler and the OpenNext build.
// Wrangler runs on the user's machine with its own OAuth login; Railyard never
// sees a Cloudflare key.

import { spawn, spawnSync } from "node:child_process";

export const COMPAT_DATE = "2026-09-20";

export function tool(env, name) {
  if (name === "wrangler") return env.RAILYARD_WRANGLER ? [env.RAILYARD_WRANGLER] : ["npx", "--yes", "wrangler@4"];
  if (name === "opennext") return env.RAILYARD_OPENNEXT ? [env.RAILYARD_OPENNEXT] : ["npx", "--yes", "@opennextjs/cloudflare@1.15.1"];
  throw new Error(`unknown tool ${name}`);
}

/** Run and capture output; echo it live when `echo` is set (through `redactFn`). */
export function run(cmd, args, { cwd, env, echo, redactFn = (s) => s, inherit = false } = {}) {
  return new Promise((resolve) => {
    const child = spawn(cmd, args, { cwd, env, stdio: inherit ? "inherit" : ["inherit", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout?.on("data", (b) => {
      stdout += b;
      if (echo) echo(redactFn(String(b)));
    });
    child.stderr?.on("data", (b) => {
      stderr += b;
      if (echo) echo(redactFn(String(b)));
    });
    child.on("error", (err) => resolve({ code: 127, stdout, stderr: `${stderr}${err.message}` }));
    // `signal` is set instead of `code` when the child was killed (e.g. Ctrl+C on a tail);
    // callers that care about a clean stop (not a crash) can tell the two apart.
    child.on("close", (code, signal) => resolve({ code: code ?? (signal ? 0 : 1), stdout, stderr, signal }));
  });
}

export function git(cwd, args) {
  const r = spawnSync("git", args, { cwd, encoding: "utf8" });
  return r.status === 0 ? r.stdout.trim() : null;
}

export function parseDeployOutput(text) {
  const url = /https:\/\/[a-z0-9.-]+\.workers\.dev\b/i.exec(text)?.[0] ?? null;
  const versionId = /Current Version ID:\s*([0-9a-f-]{8,})/i.exec(text)?.[1] ?? null;
  return { url, versionId };
}
