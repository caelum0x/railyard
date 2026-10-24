// Ship CLI against a mocked wrangler and the real console deploy code
// (RYP-D4 acceptance 1 to 4). A real Cloudflare run is owner-run.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { main, parse } from "../src/cli.mjs";
import { detect, workerName } from "../src/detect.mjs";
import { startConsole } from "./server.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const TMP = mkdtempSync(join(tmpdir(), "railyard-cli-"));
let consoleSrv;
before(async () => {
  consoleSrv = await startConsole();
});
after(async () => {
  await consoleSrv.close();
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
  g("-c", "user.email=t@x", "-c", "user.name=t", "add", ".");
  g("-c", "user.email=t@x", "-c", "user.name=t", "commit", "-qm", "init");
  return dir;
}

function ctx(cwd, extraEnv = {}) {
  const lines = [];
  const env = {
    PATH: process.env.PATH,
    HOME: TMP,
    RAILYARD_CONFIG_DIR: join(TMP, "config"),
    RAILYARD_CONSOLE_URL: consoleSrv.url,
    RAILYARD_WRANGLER: join(HERE, "fixtures/fake-wrangler.mjs"),
    RAILYARD_OPENNEXT: join(HERE, "fixtures/fake-opennext.mjs"),
    FAKE_WRANGLER_LOG: join(TMP, "wrangler.log"),
    FAKE_WRANGLER_STATE: join(TMP, "wrangler-state.json"),
    ...extraEnv,
  };
  let t = 1_000;
  return {
    lines,
    c: { env, cwd, out: (s) => lines.push(s), err: (s) => lines.push(`ERR ${s}`), now: () => (t += 700), fetchImpl: fetch, pollScale: 0 },
  };
}

const wranglerCalls = () => readFileSync(join(TMP, "wrangler.log"), "utf8").trim().split("\n").map((l) => JSON.parse(l));

test("framework detection and one-line refusals", () => {
  const d = (files) => detect(project(`d-${Math.random().toString(36).slice(2)}`, files));
  assert.equal(d({ "package.json": { dependencies: { next: "15" } } }).framework, "nextjs");
  assert.equal(d({ "package.json": { dependencies: { astro: "5" }, scripts: { build: "astro build" } } }).framework, "astro");
  assert.equal(d({ "package.json": { devDependencies: { vite: "6" }, scripts: { build: "vite build" } } }).framework, "vite");
  assert.equal(d({ "index.html": "<h1>hi</h1>" }).framework, "static");
  for (const [dep, label] of [["@sveltejs/kit", "SvelteKit"], ["@remix-run/react", "Remix"], ["nuxt", "Nuxt"]]) {
    const r = d({ "package.json": { devDependencies: { vite: "6", [dep]: "1" }, scripts: { build: "x" } } });
    assert.equal(r.ok, false);
    assert.match(r.message, new RegExp(label));
    assert.ok(!r.message.includes("\n"), "one line");
  }
  const astroServer = d({ "package.json": { dependencies: { astro: "5" }, scripts: { build: "astro build" } }, "astro.config.mjs": "export default { output: 'server' }" });
  assert.match(astroServer.message, /output: "static"/);
  const none = d({ "package.json": { dependencies: { express: "4" } } });
  assert.match(none.message, /^Railyard deploys Next\.js, Vite, Astro or static sites/);
});

test("preview names: <app> on main, <app>-<branch> elsewhere, valid Worker names", () => {
  assert.equal(workerName("Shop", "main"), "shop");
  assert.equal(workerName("shop", "feat/New Checkout"), "shop-feat-new-checkout");
  assert.ok(workerName("a".repeat(50), "b".repeat(50)).length <= 63);
});

test("no option takes a key or token", () => {
  for (const f of ["--token", "--api-key", "--key", "--cloudflare-token"]) assert.match(parse(["deploy", f, "x"]).error, /Unknown option/);
});

test("login (device flow) -> deploy main and a branch preview -> deploys recorded -> rollback", async () => {
  // Login: the code page is printed; the token is saved 0600 and never printed.
  const l = ctx(TMP);
  assert.equal(await main(["login"], l.c), 0, l.lines.join("\n"));
  const creds = JSON.parse(readFileSync(join(TMP, "config/credentials.json"), "utf8"));
  assert.match(creds.token, /^ry_/);
  assert.equal(statSync(join(TMP, "config/credentials.json")).mode & 0o777, 0o600);
  assert.ok(!l.lines.join("\n").includes(creds.token), "token never printed");
  assert.match(l.lines[0], /^Open http:\/\/127\.0\.0\.1:\d+\/device and enter [A-Z]{4}-[A-Z]{4}$/);

  const dir = project("shop", { "package.json": { name: "shop", devDependencies: { vite: "6" }, scripts: { build: "node build.mjs" } }, "build.mjs": "import {mkdirSync,writeFileSync} from 'node:fs';mkdirSync('dist',{recursive:true});writeFileSync('dist/index.html','<h1>shop</h1>')" });
  // First deploy: wrangler is logged out, so its own browser login runs first.
  const d1 = ctx(dir, { FAKE_WRANGLER_N: "1" });
  assert.equal(await main(["deploy"], d1.c), 0, d1.lines.join("\n"));
  assert.ok(d1.lines.includes("Live: https://shop.founder.workers.dev"));
  const calls = wranglerCalls();
  assert.deepEqual(calls.map((c) => c.cmd), ["whoami", "login", "deploy"]);
  assert.deepEqual(calls[2].rest, ["--name", "shop", "--assets", "dist", "--compatibility-date", "2026-09-20"]);

  execFileSync("git", ["checkout", "-qb", "feat/pay"], { cwd: dir });
  const d2 = ctx(dir, { FAKE_WRANGLER_N: "2" });
  assert.equal(await main(["deploy"], d2.c), 0, d2.lines.join("\n"));
  assert.ok(d2.lines.includes("Live: https://shop-feat-pay.founder.workers.dev"));

  execFileSync("git", ["checkout", "-q", "main"], { cwd: dir });
  const d3 = ctx(dir, { FAKE_WRANGLER_N: "3" });
  assert.equal(await main(["deploy"], d3.c), 0);

  const deploys = await consoleSrv.repo.listDeploys(consoleSrv.org.id);
  assert.equal(deploys.length, 3);
  const main1 = deploys.find((x) => x.version_id?.startsWith("00000001"));
  const sha = execFileSync("git", ["rev-parse", "HEAD"], { cwd: dir, encoding: "utf8" }).trim();
  assert.deepEqual(
    [main1.url, main1.commit_sha, main1.branch, main1.framework, main1.status, typeof main1.duration_ms],
    ["https://shop.founder.workers.dev", sha, "main", "vite", "succeeded", "number"],
  );
  assert.ok(deploys.some((x) => x.branch === "feat/pay" && x.url === "https://shop-feat-pay.founder.workers.dev"));

  // Rollback redeploys the previous version id of the same branch.
  const r = ctx(dir);
  assert.equal(await main(["rollback"], r.c), 0, r.lines.join("\n"));
  const rb = wranglerCalls().at(-1);
  assert.equal(rb.cmd, "rollback");
  assert.equal(rb.rest[0], main1.version_id);
  assert.deepEqual(rb.rest.slice(1, 3), ["--name", "shop"]);
  assert.ok(rb.rest.includes("--yes"));
  const latest = (await consoleSrv.repo.listDeploys(consoleSrv.org.id))[0];
  assert.equal(latest.version_id, main1.version_id);
  assert.ok(latest.rollback_of, "the record points at the deploy it replaced");

  // Nothing any command printed contains the token, and every console call carried it only as a header.
  const all = [...d1.lines, ...d2.lines, ...d3.lines, ...r.lines].join("\n");
  assert.ok(!all.includes(creds.token));
  for (const s of consoleSrv.seen) assert.ok(!s.url.includes(creds.token) && !s.body.includes(creds.token));
});

test("Next.js builds through OpenNext and deploys the worker with its assets", async () => {
  const dir = project("web", { "package.json": { name: "web", dependencies: { next: "14.2.35" } } });
  const c = ctx(dir, { FAKE_WRANGLER_LOGGED_IN: "1" });
  assert.equal(await main(["deploy", "--app", "web"], c.c), 0, c.lines.join("\n"));
  const calls = wranglerCalls();
  const i = calls.map((x) => x.cmd).lastIndexOf("opennext");
  assert.deepEqual(calls[i].rest, ["build"]);
  assert.deepEqual(calls[i + 1].rest.slice(0, 4), [".open-next/worker.js", "--name", "web", "--assets"]);
  assert.ok(calls[i + 1].rest.includes("nodejs_compat"));
});

test("a failed deploy is recorded as failed and exits 1; unsupported exits 2 with one line", async () => {
  const dir = project("site", { "index.html": "<h1>x</h1>" });
  const c = ctx(dir, { FAKE_WRANGLER_LOGGED_IN: "1", FAKE_WRANGLER_FAIL: "1" });
  assert.equal(await main(["deploy"], c.c), 1);
  const last = (await consoleSrv.repo.listDeploys(consoleSrv.org.id)).find((d) => d.status === "failed");
  assert.equal(last?.framework, "static");
  const kit = project("kit", { "package.json": { devDependencies: { "@sveltejs/kit": "2", vite: "6" } } });
  const k = ctx(kit, { FAKE_WRANGLER_LOGGED_IN: "1" });
  assert.equal(await main(["deploy"], k.c), 2);
  assert.equal(k.lines.length, 1);
  assert.match(k.lines[0], /^ERR Railyard deploys Next\.js, Vite, Astro or static sites; this is SvelteKit/);
});

test("deploy without login says what to do", async () => {
  const c = ctx(TMP, { RAILYARD_CONFIG_DIR: join(TMP, "nowhere") });
  assert.equal(await main(["deploy"], c.c), 1);
  assert.deepEqual(c.lines, ["ERR Run railyard login first."]);
});

// `railyard logs` wraps `wrangler tail`, a stream that can run for hours. These
// load-test it the way the PLAN-2026-10-23-DEEP.md addendum flagged as not yet
// done: a long stream of lines (so a naive implementation that buffers the
// whole thing would show it) and a Ctrl+C-style signal stop mid-stream.
test("logs: a long tail streams every line live, redacts the saved token, and never fails on a normal end", async () => {
  const { token } = JSON.parse(readFileSync(join(TMP, "config/credentials.json"), "utf8"));
  const dir = project("logger", { "index.html": "<h1>x</h1>" });
  const lines = 4000;
  const c = ctx(dir, { FAKE_WRANGLER_LOGGED_IN: "1", FAKE_WRANGLER_TAIL_LINES: String(lines), FAKE_WRANGLER_TAIL_SECRET: token });
  assert.equal(await main(["logs", "--format", "json", "--status", "error"], c.c), 0, c.lines.join("\n"));
  assert.equal(c.lines[0], "Streaming logger. Stop with Ctrl+C.");
  // Every line wrangler printed reached the terminal (nothing dropped by capture:false)...
  const streamed = c.lines.slice(1).join("\n");
  assert.equal((streamed.match(/"line":/g) ?? []).length, lines);
  // ...the saved token never appears in the clear...
  assert.ok(!streamed.includes(token), "token redacted");
  assert.ok(streamed.includes("[redacted]"), "redaction placeholder present");
  const call = wranglerCalls().at(-1);
  assert.equal(call.cmd, "tail");
  assert.deepEqual(call.rest, ["--name", "logger", "--format", "json", "--status", "error"]);
});

test("logs: a Ctrl+C-style signal mid-stream ends the command cleanly, not as a failure", async () => {
  const dir = project("logger2", { "index.html": "<h1>x</h1>" });
  const c = ctx(dir, { FAKE_WRANGLER_LOGGED_IN: "1", FAKE_WRANGLER_TAIL_LINES: "3", FAKE_WRANGLER_TAIL_SELF_SIGTERM: "1" });
  assert.equal(await main(["logs"], c.c), 0, c.lines.join("\n"));
  assert.ok(!c.lines.some((l) => l.startsWith("ERR")), "a signal stop is not reported as an error");
});
