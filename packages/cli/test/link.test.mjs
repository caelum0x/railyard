// `railyard link` and `railyard ci` (Wave P PY3.1) against a stand-in console
// and GitHub's OIDC endpoint, with the mocked wrangler. The console side of the
// same contract is tested in apps/console/lib/ship/ship.test.ts.
import { test, before, after } from "node:test";
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { main } from "../src/cli.mjs";
import { githubRepo } from "../src/link/github.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const TMP = mkdtempSync(join(tmpdir(), "railyard-link-"));
const CF_TOKEN = "cf-access-short-lived-0123456789";
const RY_TOKEN = `ry_${"k".repeat(30)}`;
let srv;
let base;
const seen = [];
const state = { polls: 0, finalState: "linked", oidcCount: 0 };

before(async () => {
  srv = createServer(async (req, res) => {
    let raw = "";
    for await (const c of req) raw += c;
    const url = new URL(req.url, "http://x");
    seen.push({ method: req.method, path: url.pathname, query: url.search, auth: req.headers.authorization ?? null, body: raw });
    const send = (status, body) => {
      res.writeHead(status, { "content-type": "application/json" });
      res.end(JSON.stringify(body));
    };
    if (url.pathname === "/oidc") {
      if (req.headers.authorization !== "Bearer actions-request-token") return send(401, {});
      state.oidcCount += 1;
      return send(200, { value: `oidc.jwt.${state.oidcCount}-${url.searchParams.get("audience").length}` });
    }
    if (url.pathname === "/api/v1/github/link" && req.method === "POST") {
      if (req.headers.authorization !== `Bearer ${RY_TOKEN}`) return send(401, { error: "unauthorized" });
      return send(201, { id: "sess-0123456789abcdefghij", url: `${base}/api/v1/github/link/go?s=sess-0123456789abcdefghij`, expires_at: "x" });
    }
    if (url.pathname === "/api/v1/github/link/status") {
      state.polls += 1;
      if (state.polls < 3) return send(200, { state: "started", message: null });
      return send(200, state.finalState === "linked"
        ? { state: "linked", message: null, repo: "acme/shop", app: "shop", pr_url: "https://github.com/acme/shop/pull/1", production_branch: "main" }
        : { state: "failed", message: "Cloudflare access was not granted." });
    }
    if (url.pathname === "/api/v1/github/ci/session") {
      if (!/^Bearer oidc\.jwt\.\d+-\d+$/.test(req.headers.authorization ?? "")) return send(401, { error: "unauthorized" });
      return send(200, { app: "shop", branch: "feature-x", production: false, production_branch: "main", cloudflare: { account_id: "0123456789abcdef0123456789abcdef", access_token: CF_TOKEN, expires_in: 3600 } });
    }
    if (url.pathname === "/api/v1/github/ci/report") {
      if (!/^Bearer oidc\.jwt\.\d+-\d+$/.test(req.headers.authorization ?? "")) return send(401, { error: "unauthorized" });
      return send(200, { deploy: { id: "d1" }, comment_url: "https://github.com/acme/shop/pull/7#issuecomment-1" });
    }
    send(404, { error: "not found" });
  });
  await new Promise((r) => srv.listen(0, "127.0.0.1", r));
  base = `http://127.0.0.1:${srv.address().port}`;
  mkdirSync(join(TMP, "config"), { recursive: true });
  writeFileSync(join(TMP, "config/credentials.json"), JSON.stringify({ console: base, token: RY_TOKEN, org: "o1" }), { mode: 0o600 });
});
after(async () => {
  await new Promise((r) => srv.close(r));
  rmSync(TMP, { recursive: true, force: true });
});

function repoDir(name, remote) {
  const dir = join(TMP, name);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "index.html"), "<h1>shop</h1>");
  const g = (...a) => execFileSync("git", a, { cwd: dir, stdio: "ignore" });
  g("init", "-q", "-b", "main");
  if (remote) g("remote", "add", "origin", remote);
  g("-c", "user.email=t@x", "-c", "user.name=t", "add", ".");
  g("-c", "user.email=t@x", "-c", "user.name=t", "commit", "-qm", "init");
  return dir;
}

function ctx(cwd, env = {}) {
  const lines = [];
  let t = 0;
  return {
    lines,
    c: {
      env: { PATH: process.env.PATH, HOME: TMP, RAILYARD_CONFIG_DIR: join(TMP, "config"), RAILYARD_CONSOLE_URL: base, RAILYARD_WRANGLER: join(HERE, "fixtures/fake-wrangler.mjs"), FAKE_WRANGLER_LOG: join(TMP, "wrangler.log"), ...env },
      cwd,
      out: (s) => lines.push(s),
      err: (s) => lines.push(`ERR ${s}`),
      now: () => (t += 1000),
      fetchImpl: fetch,
      pollScale: 0,
    },
  };
}

test("GitHub remotes: https, ssh, with .git, with credentials in the URL; others are not GitHub", () => {
  assert.equal(githubRepo("https://github.com/acme/shop.git"), "acme/shop");
  assert.equal(githubRepo("git@github.com:acme/shop.git"), "acme/shop");
  assert.equal(githubRepo("ssh://git@github.com/acme/shop"), "acme/shop");
  assert.equal(githubRepo("https://x-access-token:abc@github.com/acme/shop.git"), "acme/shop");
  assert.equal(githubRepo("https://gitlab.com/acme/shop.git"), null);
  assert.equal(githubRepo(null), null);
});

test("link: one page to open, then it waits and says which pull request to merge; no token printed", async () => {
  const dir = repoDir("shop", "git@github.com:acme/shop.git");
  const c = ctx(dir);
  assert.equal(await main(["link"], c.c), 0, c.lines.join("\n"));
  assert.equal(c.lines[0], `Open ${base}/api/v1/github/link/go?s=sess-0123456789abcdefghij`);
  assert.match(c.lines.at(-1), /^Linked acme\/shop to shop\. Merge https:\/\/github\.com\/acme\/shop\/pull\/1; after that a push to main deploys/);
  const start = seen.find((s) => s.path === "/api/v1/github/link");
  assert.deepEqual(JSON.parse(start.body), { repo: "acme/shop", app: "shop" });
  assert.ok(!c.lines.join("\n").includes(RY_TOKEN));
  for (const s of seen) assert.ok(!s.query.includes(RY_TOKEN) && !s.body.includes(RY_TOKEN));
});

test("link: a refused grant says why; a folder without a GitHub remote is refused before any call", async () => {
  state.polls = 0;
  state.finalState = "failed";
  const c = ctx(repoDir("shop2", "https://github.com/acme/shop2"));
  assert.equal(await main(["link"], c.c), 1);
  assert.equal(c.lines.at(-1), "ERR Not linked: Cloudflare access was not granted.");
  const before = seen.length;
  const n = ctx(repoDir("local", null));
  assert.equal(await main(["link"], n.c), 2);
  assert.match(n.lines[0], /no GitHub remote named origin/);
  assert.equal(seen.length, before);
});

test("ci: OIDC in, a masked Cloudflare token to wrangler, the result reported with a fresh OIDC token, the preview link printed", async () => {
  const dir = repoDir("ci-shop", "https://github.com/acme/shop");
  const env = { GITHUB_ACTIONS: "true", ACTIONS_ID_TOKEN_REQUEST_URL: `${base}/oidc?api-version=2.0`, ACTIONS_ID_TOKEN_REQUEST_TOKEN: "actions-request-token", FAKE_WRANGLER_LOGGED_IN: "1" };
  state.oidcCount = 0;
  const c = ctx(dir, env);
  assert.equal(await main(["ci"], c.c), 0, c.lines.join("\n"));
  // The mask comes before anything else that could echo the token.
  assert.equal(c.lines[0], `::add-mask::${CF_TOKEN}`);
  assert.ok(c.lines.slice(1).every((l) => !l.includes(CF_TOKEN)), "the token is printed only in the mask command");
  assert.ok(c.lines.includes("Deploying shop from feature-x (preview)"));
  assert.ok(c.lines.includes("Preview posted: https://github.com/acme/shop/pull/7#issuecomment-1"));
  // Wrangler ran with the preview name and never tried a browser login.
  const calls = readFileSync(join(TMP, "wrangler.log"), "utf8").trim().split("\n").map((l) => JSON.parse(l));
  assert.deepEqual(calls.map((x) => x.cmd), ["whoami", "deploy"]);
  assert.equal(calls[1].rest[calls[1].rest.indexOf("--name") + 1], "shop-feature-x");
  const report = seen.filter((s) => s.path === "/api/v1/github/ci/report").at(-1);
  const body = JSON.parse(report.body);
  assert.deepEqual(Object.keys(body).sort(), ["duration_ms", "framework", "status", "url", "version_id"]);
  assert.equal(body.url, "https://shop-feature-x.founder.workers.dev");
  assert.equal(state.oidcCount, 2, "a fresh identity token for the report");
  const oidc = seen.filter((s) => s.path === "/oidc");
  assert.ok(oidc.every((s) => new URL(`http://x${s.query}`).searchParams.get("audience") === base));
  assert.ok(!report.body.includes(CF_TOKEN));
});

test("ci: outside Actions, or without id-token permission, it stops with one line; a refused token never opens a login", async () => {
  const dir = repoDir("ci-shop2", "https://github.com/acme/shop");
  const a = ctx(dir);
  assert.equal(await main(["ci"], a.c), 2);
  assert.match(a.lines[0], /inside GitHub Actions only/);
  const b = ctx(dir, { GITHUB_ACTIONS: "true" });
  assert.equal(await main(["ci"], b.c), 2);
  assert.match(b.lines[0], /id-token: write/);
  const env = { GITHUB_ACTIONS: "true", ACTIONS_ID_TOKEN_REQUEST_URL: `${base}/oidc`, ACTIONS_ID_TOKEN_REQUEST_TOKEN: "actions-request-token", FAKE_WRANGLER_STATE: join(TMP, "never-logged-in.json") };
  const r = ctx(dir, env);
  assert.equal(await main(["ci"], r.c), 1);
  assert.ok(r.lines.includes("ERR Cloudflare refused the access token. Run railyard link again to renew the grant."));
  const calls = readFileSync(join(TMP, "wrangler.log"), "utf8").trim().split("\n").map((l) => JSON.parse(l));
  assert.notEqual(calls.at(-1).cmd, "login");
});
