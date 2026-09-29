// PY1.2 (Railyard side): deploy from a Scribase project folder with the project's
// URL and anon key set at build and run time, never printed, never pasted; the
// Scribase CLI links this machine with a one-time claim read from stdin.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { main } from "../src/cli.mjs";
import { projectEnv, wranglerVars } from "../src/scribase/env.mjs";
import { startConsole } from "./server.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const TMP = mkdtempSync(join(tmpdir(), "railyard-sb-"));
const ANON = "eyJhbGciOiJIUzI1NiJ9.eyJyb2xlIjoiYW5vbiJ9.c2lnbmF0dXJlLWFub24ta2V5LXZhbHVl";
const SERVICE = "eyJhbGciOiJIUzI1NiJ9.eyJyb2xlIjoic2VydmljZV9yb2xlIn0.c2lnbmF0dXJlLXNlcnZpY2Uta2V5";
let srv;
before(async () => {
  srv = await startConsole();
});
after(async () => {
  await srv.close();
  rmSync(TMP, { recursive: true, force: true });
});

function project(name, files, branch = "main") {
  const dir = join(TMP, name);
  mkdirSync(dir, { recursive: true });
  for (const [f, body] of Object.entries(files)) {
    mkdirSync(dirname(join(dir, f)), { recursive: true });
    writeFileSync(join(dir, f), typeof body === "string" ? body : JSON.stringify(body));
  }
  const g = (...a) => execFileSync("git", a, { cwd: dir, stdio: "ignore" });
  g("init", "-q", "-b", branch);
  g("-c", "user.email=t@x", "-c", "user.name=t", "add", "-f", ".");
  g("-c", "user.email=t@x", "-c", "user.name=t", "commit", "-qm", "init");
  return dir;
}

function ctx(cwd, extraEnv = {}) {
  const lines = [];
  const env = {
    PATH: process.env.PATH,
    HOME: TMP,
    RAILYARD_CONFIG_DIR: join(TMP, "config"),
    RAILYARD_CONSOLE_URL: srv.url,
    RAILYARD_WRANGLER: join(HERE, "fixtures/fake-wrangler.mjs"),
    RAILYARD_OPENNEXT: join(HERE, "fixtures/fake-opennext.mjs"),
    FAKE_WRANGLER_LOG: join(TMP, "wrangler.log"),
    FAKE_WRANGLER_LOGGED_IN: "1",
    ...extraEnv,
  };
  let t = 1_000;
  return { lines, c: { env, cwd, out: (s) => lines.push(s), err: (s) => lines.push(`ERR ${s}`), now: () => (t += 500), fetchImpl: fetch, pollScale: 0 } };
}
// Async: the console under test runs in this process, so a blocking spawn would deadlock.
function runBin(args, { input = "", env }) {
  return new Promise((resolve) => {
    const child = spawn(process.execPath, [join(HERE, "../bin/railyard.mjs"), ...args], { env });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (b) => (stdout += b));
    child.stderr.on("data", (b) => (stderr += b));
    child.on("close", (status) => resolve({ status, stdout, stderr }));
    child.stdin.end(input);
  });
}
const calls = () => readFileSync(join(TMP, "wrangler.log"), "utf8").trim().split("\n").map((l) => JSON.parse(l));

test("projectEnv reads the public URL and anon key that scribase init wrote, and refuses secrets by name", () => {
  const dir = project("envs", {
    ".env.local": `# Scribase\nNEXT_PUBLIC_SUPABASE_URL=https://abc.scribase.app\nNEXT_PUBLIC_SUPABASE_ANON_KEY=${ANON}\nSUPABASE_SERVICE_ROLE_KEY=${SERVICE}\nOTHER=1\n`,
    ".env": "VITE_SUPABASE_URL=https://old.example\n",
  });
  const e = projectEnv(dir);
  assert.equal(e.found, true);
  assert.deepEqual(Object.keys(e.values).sort(), ["NEXT_PUBLIC_SUPABASE_ANON_KEY", "NEXT_PUBLIC_SUPABASE_URL", "VITE_SUPABASE_URL"]);
  assert.deepEqual(e.refused, ["SUPABASE_SERVICE_ROLE_KEY"]);
  assert.equal(projectEnv(project("plain", { "index.html": "x" })).found, false);
  // A service-role JWT under a public-looking name is refused too.
  const sneaky = project("sneaky", { ".env": `VITE_SUPABASE_ANON_KEY=${SERVICE}\nVITE_SUPABASE_URL=https://x.scribase.app\n` });
  assert.deepEqual(projectEnv(sneaky).refused, ["VITE_SUPABASE_ANON_KEY"]);
  assert.deepEqual(wranglerVars({ A: "1", B: "two words" }), ["--var", "A:1", "--var", "B:two words"]);
});

test("a branch preview uses the Scribase branch's settings when the branch file exists", () => {
  const dir = project("prev", {
    ".env.local": `VITE_SUPABASE_URL=https://main.scribase.app\nVITE_SUPABASE_ANON_KEY=${ANON}\n`,
    ".scribase/branches/feat-pay.env": `VITE_SUPABASE_URL=https://feat-pay.scribase.app\nVITE_SUPABASE_ANON_KEY=${ANON}\n`,
  });
  assert.equal(projectEnv(dir, "feat/pay").values.VITE_SUPABASE_URL, "https://feat-pay.scribase.app");
  assert.equal(projectEnv(dir, "feat/pay").branch, true);
  assert.equal(projectEnv(dir, "main").values.VITE_SUPABASE_URL, "https://main.scribase.app");
  assert.equal(projectEnv(dir, "other").branch, false);
});

test("link-scribase exchanges a one-time claim read from stdin and saves credentials 0600; nothing is printed", async () => {
  const claim = await srv.claim();
  const env = { ...process.env, RAILYARD_CONFIG_DIR: join(TMP, "linked"), RAILYARD_CONSOLE_URL: srv.url };
  const r = await runBin(["link-scribase"], { input: JSON.stringify({ claim, console: srv.url }), env });
  assert.equal(r.status, 0, r.stderr);
  const creds = JSON.parse(readFileSync(join(TMP, "linked/credentials.json"), "utf8"));
  assert.match(creds.token, /^ry_/);
  assert.equal(creds.source, "scribase");
  assert.equal(statSync(join(TMP, "linked/credentials.json")).mode & 0o777, 0o600);
  assert.ok(!r.stdout.includes(creds.token) && !r.stderr.includes(creds.token));
  assert.ok(!r.stdout.includes(claim));
  const again = await runBin(["link-scribase"], { input: JSON.stringify({ claim, console: srv.url }), env });
  assert.equal(again.status, 1, "a claim works once");
  const argvClaim = await runBin(["link-scribase", "--claim", claim], { env });
  assert.equal(argvClaim.status, 2, "no flag takes the claim");
});

test("deploy in a Scribase project folder sets the URL and anon key for the build and the Worker, and never prints the key", async () => {
  const l = ctx(TMP);
  assert.equal(await main(["login"], l.c), 0);
  const build = "import {mkdirSync,writeFileSync} from 'node:fs';mkdirSync('dist',{recursive:true});writeFileSync('dist/index.html','<p>'+process.env.VITE_SUPABASE_URL+'|'+(process.env.VITE_SUPABASE_ANON_KEY?'key-set':'no-key')+'</p>')";
  const dir = project("sbapp", {
    "package.json": { name: "sbapp", devDependencies: { vite: "6" }, scripts: { build: "node build.mjs" } },
    "build.mjs": build,
    ".env.local": `VITE_SUPABASE_URL=https://sbapp.scribase.app\nVITE_SUPABASE_ANON_KEY=${ANON}\nSUPABASE_SERVICE_ROLE_KEY=${SERVICE}\n`,
  });
  const d = ctx(dir);
  assert.equal(await main(["deploy"], d.c), 0, d.lines.join("\n"));
  assert.equal(readFileSync(join(dir, "dist/index.html"), "utf8"), "<p>https://sbapp.scribase.app|key-set</p>");
  const dep = calls().at(-1);
  assert.equal(dep.cmd, "deploy");
  assert.ok(dep.rest.includes(`VITE_SUPABASE_URL:https://sbapp.scribase.app`));
  assert.ok(dep.rest.includes(`VITE_SUPABASE_ANON_KEY:${ANON}`));
  assert.ok(!dep.rest.join(" ").includes(SERVICE), "the service role key never reaches the Worker");
  const out = d.lines.join("\n");
  assert.match(out, /Using the Scribase project settings: VITE_SUPABASE_ANON_KEY, VITE_SUPABASE_URL/);
  assert.match(out, /Left out as secrets: SUPABASE_SERVICE_ROLE_KEY/);
  assert.ok(!out.includes(ANON) && !out.includes(SERVICE), "no key is printed");
});
