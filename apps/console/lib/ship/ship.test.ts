// Ship on push (Wave P PY3.1, RYP-E3, ADR RY-0004): link in two browser steps,
// the workflow pull request, the OIDC-to-Cloudflare exchange, deploy records,
// preview comments, uninstall webhooks, and no token on any screen.
//   node --test lib/ship/ship.test.ts
import { test } from "node:test";
import assert from "node:assert/strict";
import { createHmac, createVerify, generateKeyPairSync } from "node:crypto";

import { MemoryRepo } from "../repo.ts";
import { emptyEntitlement } from "../types.ts";
import { b64url } from "../crypto.ts";
import { CloudflareOAuth, pkceChallenge } from "./cloudflare.ts";
import { GithubApp } from "./github.ts";
import { fromB64url, pkcs1ToPkcs8, signRs256, verifyRs256, type Jwk } from "./jwt.ts";
import { MemoryShipStore } from "./store.ts";
import { ciReport, ciSession, cloudflareCallback, githubSetup, go, linkStatus, ShipError, startLink, webhook, type ShipDeps } from "./service.ts";
import { ciSessionH, goH, linkStartH, page, webhookH } from "./http.ts";
import { WORKFLOW_PATH, workflowYaml } from "./workflow.ts";
import { manifest, manifestDone } from "./manifest.ts";

const T0 = new Date("2026-09-30T10:00:00Z");
const CONSOLE = "https://railyard.example";
const CREDS_KEY = Buffer.alloc(32, 7).toString("base64");
const REPO_ID = 4242;
const FULL = "acme/shop";
const INST = 777;
const CF_ACCOUNT = "0123456789abcdef0123456789abcdef";

// ---- keys: the GitHub App (PKCS#1, as GitHub issues it) and GitHub's OIDC signer ----
const app = generateKeyPairSync("rsa", { modulusLength: 2048 });
const APP_PEM = app.privateKey.export({ type: "pkcs1", format: "pem" }).toString();
const oidcPair = await crypto.subtle.generateKey({ name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" }, true, ["sign", "verify"]);
const oidcJwk = { ...(await crypto.subtle.exportKey("jwk", oidcPair.publicKey)), kid: "gh1" } as Jwk;

async function oidc(claims: Record<string, unknown> = {}, opts: { key?: CryptoKey; kid?: string } = {}) {
  const iat = Math.floor(T0.getTime() / 1000);
  return signRs256(
    opts.key ?? oidcPair.privateKey,
    {
      iss: "https://token.actions.githubusercontent.com",
      aud: CONSOLE,
      iat,
      nbf: iat,
      exp: iat + 300,
      repository: FULL,
      repository_id: String(REPO_ID),
      repository_owner: "acme",
      ref: "refs/heads/main",
      sha: "a".repeat(40),
      event_name: "push",
      job_workflow_ref: `${FULL}/${WORKFLOW_PATH}@refs/heads/main`,
      ...claims,
    },
    opts.kid ?? "gh1",
  );
}

// ---- a fake GitHub and a fake Cloudflare behind one fetch ----
function fakes() {
  const gh = {
    installedOn: new Set<string>(),
    files: new Map<string, string>(), // "branch:path" -> content
    refs: new Map<string, string>([["main", "sha-main"]]),
    prs: [] as { number: number; head: string; html_url: string }[],
    comments: [] as { id: number; pr: number; body: string }[],
    calls: [] as string[],
    tokensIssued: 0,
  };
  const cf = { codes: new Map<string, string>(), refreshes: 0, refreshToken: "cf-refresh-1", lastForm: null as URLSearchParams | null, rotate: true };
  const reply = (status: number, body?: unknown) => new Response(body === undefined ? "" : JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  const fetchImpl = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input));
    const method = init?.method ?? "GET";
    const auth = new Headers(init?.headers).get("authorization") ?? "";
    const body = init?.body ? String(init.body) : "";
    if (url.host === "api.github.com") {
      gh.calls.push(`${method} ${url.pathname}`);
      const p = url.pathname;
      if (p === `/repos/${FULL}/installation`) {
        // The App JWT must verify with the App's public key (PKCS#1 key converted for WebCrypto).
        const jwt = auth.replace(/^Bearer /, "");
        const [h, c, s] = jwt.split(".");
        const ok = createVerify("RSA-SHA256").update(`${h}.${c}`).verify(app.publicKey, Buffer.from(fromB64url(s as string)));
        if (!ok) return reply(401);
        return gh.installedOn.has(FULL) ? reply(200, { id: INST, account: { login: "acme" } }) : reply(404);
      }
      if (p === `/app/installations/${INST}/access_tokens`) return (gh.tokensIssued++, reply(201, { token: `ghs_install_${gh.tokensIssued}` }));
      if (!auth.startsWith("Bearer ghs_install_")) return reply(401);
      if (p === `/repos/${FULL}`) return reply(200, { id: REPO_ID, full_name: FULL, default_branch: "main", owner: { login: "acme" } });
      if (p === `/repos/${FULL}/contents/${WORKFLOW_PATH}`) {
        const ref = url.searchParams.get("ref") ?? JSON.parse(body || "{}").branch;
        if (method === "GET") {
          const c = gh.files.get(`${ref}:${WORKFLOW_PATH}`);
          return c === undefined ? reply(404) : reply(200, { content: Buffer.from(c).toString("base64"), sha: "blob1" });
        }
        const b = JSON.parse(body);
        gh.files.set(`${b.branch}:${WORKFLOW_PATH}`, Buffer.from(b.content, "base64").toString());
        return reply(201, {});
      }
      if (p === `/repos/${FULL}/pulls` && method === "GET") {
        const head = url.searchParams.get("head")?.split(":")[1];
        return reply(200, gh.prs.filter((x) => x.head === head));
      }
      if (p === `/repos/${FULL}/pulls` && method === "POST") {
        const b = JSON.parse(body);
        const pr = { number: gh.prs.length + 1, head: b.head, html_url: `https://github.com/${FULL}/pull/${gh.prs.length + 1}` };
        gh.prs.push(pr);
        return reply(201, pr);
      }
      const ref = /^\/repos\/acme\/shop\/git\/ref\/heads\/(.+)$/.exec(p);
      if (ref) return gh.refs.has(decodeURIComponent(ref[1] as string)) ? reply(200, { object: { sha: gh.refs.get(decodeURIComponent(ref[1] as string)) } }) : reply(404);
      if (p === `/repos/${FULL}/git/refs` && method === "POST") {
        const b = JSON.parse(body);
        gh.refs.set(b.ref.replace("refs/heads/", ""), b.sha);
        return reply(201, {});
      }
      const issue = /^\/repos\/acme\/shop\/issues\/(\d+)\/comments$/.exec(p);
      if (issue && method === "GET") return reply(200, gh.comments.filter((x) => x.pr === Number(issue[1])).map((x) => ({ ...x, html_url: `https://github.com/${FULL}/pull/${x.pr}#issuecomment-${x.id}` })));
      if (issue && method === "POST") {
        const cm = { id: gh.comments.length + 100, pr: Number(issue[1]), body: JSON.parse(body).body };
        gh.comments.push(cm);
        return reply(201, { html_url: `https://github.com/${FULL}/pull/${cm.pr}#issuecomment-${cm.id}` });
      }
      const edit = /^\/repos\/acme\/shop\/issues\/comments\/(\d+)$/.exec(p);
      if (edit && method === "PATCH") {
        gh.comments = gh.comments.map((x) => (x.id === Number(edit[1]) ? { ...x, body: JSON.parse(body).body } : x));
        return reply(200, { html_url: `https://github.com/${FULL}/pull/1#issuecomment-${edit[1]}` });
      }
      return reply(404);
    }
    if (url.host === "cf.test" && url.pathname === "/token") {
      const form = new URLSearchParams(body);
      cf.lastForm = form;
      if (auth !== `Basic ${btoa("cf-client:cf-secret")}`) return reply(401, { error: "invalid_client" });
      if (form.get("grant_type") === "authorization_code") {
        const challenge = cf.codes.get(form.get("code") ?? "");
        if (!challenge || challenge !== (await pkceChallenge(form.get("code_verifier") ?? ""))) return reply(400, { error: "invalid_grant" });
        return reply(200, { access_token: "cf-access-1", refresh_token: cf.refreshToken, expires_in: 3600 });
      }
      if (form.get("grant_type") === "refresh_token") {
        if (form.get("refresh_token") !== cf.refreshToken) return reply(400, { error: "invalid_grant" });
        cf.refreshes += 1;
        if (cf.rotate) cf.refreshToken = `cf-refresh-${cf.refreshes + 1}`;
        return reply(200, { access_token: `cf-access-r${cf.refreshes}`, refresh_token: cf.rotate ? cf.refreshToken : undefined, expires_in: 3600 });
      }
    }
    if (url.host === "cfapi.test" && url.pathname === "/accounts") {
      return auth.startsWith("Bearer cf-access") ? reply(200, { result: [{ id: CF_ACCOUNT, name: "Acme" }] }) : reply(403);
    }
    throw new Error(`unexpected fetch ${method} ${url}`);
  }) as typeof fetch;
  return { gh, cf, fetchImpl };
}

async function setup(opts: { plan?: "addon" | "none"; active?: boolean } = {}) {
  const f = fakes();
  const repo = new MemoryRepo(() => T0);
  const store = new MemoryShipStore();
  const org = await repo.createOrgForUser({ userId: "u1", email: "founder@example.com", name: "Acme" });
  if (opts.plan !== "none") await repo.saveEntitlement({ ...emptyEntitlement(org.id, T0), plan: "addon", status: opts.active === false ? "inactive" : "active", addon_slots: ["deploy"], apps_limit: 10 });
  const deps: ShipDeps = {
    store,
    repo,
    github: new GithubApp({ appId: "12345", slug: "railyard-test", privateKey: APP_PEM, webhookSecret: "whsec-test", fetchImpl: f.fetchImpl }),
    cloudflare: new CloudflareOAuth({ clientId: "cf-client", clientSecret: "cf-secret", redirectUri: `${CONSOLE}/api/v1/github/cloudflare/callback`, scopes: ["workers:write", "offline_access"], authorizeUrl: "https://cf.test/auth", tokenUrl: "https://cf.test/token", apiBase: "https://cfapi.test", fetchImpl: f.fetchImpl }),
    oidcKeys: async () => [oidcJwk],
    consoleUrl: CONSOLE,
    credentialsKey: CREDS_KEY,
  };
  return { ...f, repo, store, deps, org: org.id };
}

/** Walks the browser half: install the App, grant Cloudflare. Returns the session id and the final message. */
async function linkThrough(s: Awaited<ReturnType<typeof setup>>) {
  const { id } = await startLink(s.deps, s.org, { repo: FULL, app: "Shop" }, T0);
  const first = await go(s.deps, id, T0);
  assert.ok("redirect" in first);
  s.gh.installedOn.add(FULL);
  const second = await githubSetup(s.deps, new URLSearchParams({ installation_id: String(INST), setup_action: "install", state: id }), T0);
  assert.ok("redirect" in second);
  const auth = new URL(second.redirect);
  const session = [...s.store.sessions.values()][0]!;
  s.cf.codes.set("code-1", auth.searchParams.get("code_challenge")!);
  const done = await cloudflareCallback(s.deps, new URLSearchParams({ code: "code-1", state: id }), T0);
  return { id, first, second, auth, session, done };
}

// ------------------------------------------------------------------ jwt ----

test("the App JWT is signed with GitHub's PKCS#1 key (converted for WebCrypto) and verifies with its public key", async () => {
  const gh = new GithubApp({ appId: "12345", slug: "x", privateKey: APP_PEM.replace(/\n/g, "\\n"), webhookSecret: "w" });
  const jwt = await gh.appJwt(T0);
  const [h, c, sig] = jwt.split(".");
  assert.ok(createVerify("RSA-SHA256").update(`${h}.${c}`).verify(app.publicKey, Buffer.from(fromB64url(sig!))));
  const claims = JSON.parse(Buffer.from(fromB64url(c!)).toString());
  assert.equal(claims.iss, "12345");
  assert.equal(claims.exp - claims.iat, 600);
  assert.ok(claims.iat < T0.getTime() / 1000);
  // The converted key is a valid PKCS#8 structure.
  const der = app.privateKey.export({ type: "pkcs1", format: "der" });
  assert.deepEqual(Buffer.from(pkcs1ToPkcs8(new Uint8Array(der))), app.privateKey.export({ type: "pkcs8", format: "der" }));
});

test("OIDC verification refuses a wrong audience, issuer, key, algorithm, expiry or a tampered body", async () => {
  const keys = [oidcJwk];
  const opts = { issuer: "https://token.actions.githubusercontent.com", audience: CONSOLE, now: T0 };
  assert.equal((await verifyRs256(await oidc(), keys, opts)).ok, true);
  assert.deepEqual(await verifyRs256(await oidc({ aud: "https://evil.example" }), keys, opts), { ok: false, error: "wrong audience" });
  assert.deepEqual(await verifyRs256(await oidc({ iss: "https://evil.example" }), keys, opts), { ok: false, error: "wrong issuer" });
  assert.deepEqual(await verifyRs256(await oidc({ exp: T0.getTime() / 1000 - 120 }), keys, opts), { ok: false, error: "expired" });
  const other = await crypto.subtle.generateKey({ name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" }, true, ["sign", "verify"]);
  assert.deepEqual(await verifyRs256(await oidc({}, { key: other.privateKey }), keys, opts), { ok: false, error: "bad signature" });
  assert.deepEqual(await verifyRs256(await oidc({}, { kid: "unknown" }), keys, opts), { ok: false, error: "unknown signing key" });
  const [h, c, s] = (await oidc()).split(".");
  const forged = b64url(new TextEncoder().encode(JSON.stringify({ ...JSON.parse(Buffer.from(fromB64url(c!)).toString()), repository_id: "1" })));
  assert.deepEqual(await verifyRs256(`${h}.${forged}.${s}`, keys, opts), { ok: false, error: "bad signature" });
  const none = `${b64url(new TextEncoder().encode(JSON.stringify({ alg: "none" })))}.${c}.`;
  assert.deepEqual(await verifyRs256(none, keys, opts), { ok: false, error: "unsupported algorithm" });
});

// ----------------------------------------------------------------- link ----

test("link: two browser steps (install on the repo, grant Cloudflare), then one pull request with the workflow", async () => {
  const s = await setup();
  const { id, first, auth, done } = await linkThrough(s);
  assert.ok("redirect" in first && first.redirect === `https://github.com/apps/railyard-test/installations/new?state=${encodeURIComponent(id)}`);
  assert.equal(auth.origin + auth.pathname, "https://cf.test/auth");
  assert.equal(auth.searchParams.get("code_challenge_method"), "S256");
  assert.equal(auth.searchParams.get("scope"), "workers:write offline_access");
  assert.equal(auth.searchParams.get("redirect_uri"), `${CONSOLE}/api/v1/github/cloudflare/callback`);
  // The verifier went to the token endpoint, never into a URL.
  assert.ok(!auth.toString().includes([...s.store.sessions.values()][0]!.pkce_verifier));
  assert.match(done.done, /^Linked acme\/shop to shop\. Merge the pull request \(https:\/\/github\.com\/acme\/shop\/pull\/1\)/);
  const wf = s.gh.files.get(`railyard/ship-on-push:${WORKFLOW_PATH}`)!;
  assert.equal(wf, workflowYaml({ consoleUrl: CONSOLE, productionBranch: "main" }));
  assert.equal(s.gh.prs[0]!.head, "railyard/ship-on-push");
  assert.ok(!s.gh.calls.some((c) => c.startsWith("PUT") && c.includes("heads/main")), "nothing is pushed to the default branch");
  const status = await linkStatus(s.deps, s.org, id);
  assert.deepEqual(status, { state: "linked", message: null, repo: FULL, app: "shop", pr_url: "https://github.com/acme/shop/pull/1", production_branch: "main" });
  // The Cloudflare grant is sealed; no plaintext token in anything stored.
  const stored = JSON.stringify([...s.store.cloudflare.values(), ...s.store.sessions.values(), ...s.store.links]);
  for (const secret of ["cf-refresh-1", "cf-access-1"]) assert.ok(!stored.includes(secret), secret);
  assert.equal(s.store.cloudflare.get(s.org)!.account_id, CF_ACCOUNT);
  // Another org cannot read this link's status.
  await assert.rejects(linkStatus(s.deps, crypto.randomUUID(), id), (e: ShipError) => e.status === 404);
  // A used link cannot be replayed.
  await assert.rejects(go(s.deps, id, T0), (e: ShipError) => e.status === 409);
});

test("link: with the App installed and Cloudflare granted, the link finishes on the first open (no click)", async () => {
  const s = await setup();
  await linkThrough(s);
  s.gh.files.set(`main:${WORKFLOW_PATH}`, workflowYaml({ consoleUrl: CONSOLE, productionBranch: "main" }));
  const { id } = await startLink(s.deps, s.org, { repo: FULL, app: "shop" }, T0);
  const r = await go(s.deps, id, T0);
  assert.ok("done" in r && /The workflow is already on main; your next push there goes live\./.test(r.done));
  assert.equal(s.gh.prs.length, 1, "no second pull request");
});

test("link: bad input, a paused org, an expired link, a forged install and a refused grant", async () => {
  const s = await setup();
  await assert.rejects(startLink(s.deps, s.org, { repo: "not a repo", app: "x" }, T0), (e: ShipError) => e.status === 400);
  await assert.rejects(startLink(s.deps, s.org, { repo: FULL, app: "!!" }, T0), (e: ShipError) => e.status === 400);
  const paused = await setup({ active: false });
  await assert.rejects(startLink(paused.deps, paused.org, { repo: FULL, app: "shop" }, T0), (e: ShipError) => e.status === 402 && /renew the add-on/.test(e.message));
  const { id } = await startLink(s.deps, s.org, { repo: FULL, app: "shop" }, T0);
  await assert.rejects(go(s.deps, id, new Date(T0.getTime() + 16 * 60_000)), (e: ShipError) => e.status === 410);
  // GitHub's setup redirect names an installation, but the App is not on the repo: nothing links.
  await assert.rejects(githubSetup(s.deps, new URLSearchParams({ installation_id: "999", state: id }), T0), (e: ShipError) => e.status === 409);
  // Cloudflare before GitHub: refused.
  await assert.rejects(cloudflareCallback(s.deps, new URLSearchParams({ code: "c", state: id }), T0), (e: ShipError) => e.status === 409);
  s.gh.installedOn.add(FULL);
  await githubSetup(s.deps, new URLSearchParams({ state: id }), T0);
  await assert.rejects(cloudflareCallback(s.deps, new URLSearchParams({ error: "access_denied", state: id }), T0), (e: ShipError) => e.status === 400);
  assert.equal((await linkStatus(s.deps, s.org, id)).state, "failed");
  // A wrong PKCE verifier (a stolen code) is refused by the token endpoint.
  const { id: id2 } = await startLink(s.deps, s.org, { repo: FULL, app: "shop" }, T0);
  const r = await go(s.deps, id2, T0);
  assert.ok("redirect" in r);
  s.cf.codes.set("stolen", await pkceChallenge("a-different-verifier-of-enough-length-xxxxxxxxxxxxxx"));
  await assert.rejects(cloudflareCallback(s.deps, new URLSearchParams({ code: "stolen", state: id2 }), T0), /invalid_grant/);
});

// ------------------------------------------------------------------- ci ----

test("ci session: the Railyard workflow's OIDC token gets a Cloudflare access token; the cache avoids a refresh", async () => {
  const s = await setup();
  await linkThrough(s);
  const r = await ciSession(s.deps, `Bearer ${await oidc()}`, T0);
  assert.deepEqual(r, { app: "shop", branch: "main", production: true, production_branch: "main", cloudflare: { account_id: CF_ACCOUNT, access_token: "cf-access-1", expires_in: 3600 } });
  assert.equal(s.cf.refreshes, 0);
  // 55 minutes later the cached token has under 10 minutes left: one refresh, the rotated refresh token is kept sealed.
  const later = new Date(T0.getTime() + 55 * 60_000);
  const r2 = await ciSession(s.deps, `Bearer ${await oidc({ iat: later.getTime() / 1000, nbf: later.getTime() / 1000, exp: later.getTime() / 1000 + 300 })}`, later);
  assert.equal(r2.cloudflare.access_token, "cf-access-r1");
  assert.equal(s.cf.refreshes, 1);
  assert.ok(!JSON.stringify([...s.store.cloudflare.values()]).includes("cf-refresh-2"));
  const later2 = new Date(T0.getTime() + 125 * 60_000);
  const r3 = await ciSession(s.deps, `Bearer ${await oidc({ iat: later2.getTime() / 1000, nbf: later2.getTime() / 1000, exp: later2.getTime() / 1000 + 300 })}`, later2);
  assert.equal(r3.cloudflare.access_token, "cf-access-r2", "the rotated refresh token was used");
});

test("ci session: only the Railyard workflow, on a push to a branch of a linked repo, of an org that is not paused", async () => {
  const s = await setup();
  await linkThrough(s);
  const bad = async (claims: Record<string, unknown>, status: number) =>
    assert.rejects(ciSession(s.deps, `Bearer ${await oidc(claims)}`, T0), (e: ShipError) => e.status === status);
  await bad({ job_workflow_ref: `${FULL}/.github/workflows/other.yml@refs/heads/main` }, 403);
  await bad({ event_name: "pull_request" }, 403);
  await bad({ ref: "refs/tags/v1" }, 403);
  await bad({ repository_id: "99", repository: "acme/other", job_workflow_ref: `acme/other/${WORKFLOW_PATH}@refs/heads/main` }, 404);
  await bad({ aud: "https://elsewhere.example" }, 401);
  await assert.rejects(ciSession(s.deps, "Bearer ry_notajwt", T0), (e: ShipError) => e.status === 401);
  await assert.rejects(ciSession(s.deps, null, T0), (e: ShipError) => e.status === 401);
  await s.repo.saveEntitlement({ ...(await s.repo.getEntitlement(s.org))!, status: "inactive" });
  await bad({}, 402);
});

test("ci report: production records the deploy; a branch with a pull request gets one preview comment, edited on the next push", async () => {
  const s = await setup();
  await linkThrough(s);
  const prod = await ciReport(s.deps, `Bearer ${await oidc()}`, { status: "succeeded", url: "https://shop.acme.workers.dev", framework: "vite", version_id: "0f0e0d0c-1111", duration_ms: 41000, branch: "spoofed", commit_sha: "b".repeat(40) }, T0);
  assert.equal(prod.comment_url, null);
  const deploys = await s.repo.listDeploys(s.org);
  assert.equal(deploys.length, 1);
  assert.equal(deploys[0]!.branch, "main", "branch comes from the token, not the body");
  assert.equal(deploys[0]!.commit_sha, "a".repeat(40));
  // A feature branch with an open PR.
  s.gh.prs.push({ number: 7, head: "feature-x", html_url: `https://github.com/${FULL}/pull/7` });
  const branchClaims = { ref: "refs/heads/feature-x", job_workflow_ref: `${FULL}/${WORKFLOW_PATH}@refs/heads/feature-x` };
  const pv = await ciReport(s.deps, `Bearer ${await oidc(branchClaims)}`, { status: "succeeded", url: "https://shop-feature-x.acme.workers.dev", framework: "vite" }, T0);
  assert.match(pv.comment_url!, /pull\/7#issuecomment-/);
  await ciReport(s.deps, `Bearer ${await oidc({ ...branchClaims, sha: "c".repeat(40) })}`, { status: "succeeded", url: "https://shop-feature-x.acme.workers.dev", framework: "vite" }, T0);
  const onPr = s.gh.comments.filter((c) => c.pr === 7);
  assert.equal(onPr.length, 1, "one comment, edited");
  assert.match(onPr[0]!.body, /Preview of `feature-x` at ccccccc: https:\/\/shop-feature-x\.acme\.workers\.dev/);
  // A failed preview says so; a success without a URL is refused.
  await ciReport(s.deps, `Bearer ${await oidc(branchClaims)}`, { status: "failed" }, T0);
  assert.match(s.gh.comments.find((c) => c.pr === 7)!.body, /failed to deploy/);
  await assert.rejects(ciReport(s.deps, `Bearer ${await oidc()}`, { status: "succeeded", url: "https://evil.example" }, T0), (e: ShipError) => e.status === 400);
  // A branch without a PR: recorded, no comment.
  const lone = await ciReport(s.deps, `Bearer ${await oidc({ ref: "refs/heads/lonely", job_workflow_ref: `${FULL}/${WORKFLOW_PATH}@refs/heads/lonely` })}`, { status: "succeeded", url: "https://shop-lonely.acme.workers.dev" }, T0);
  assert.equal(lone.comment_url, null);
});

// -------------------------------------------------------------- webhook ----

test("webhook: signed events only; an uninstall or a removed repository drops its link", async () => {
  const s = await setup();
  await linkThrough(s);
  const sign = (raw: string) => `sha256=${createHmac("sha256", "whsec-test").update(raw).digest("hex")}`;
  const removed = JSON.stringify({ action: "removed", installation: { id: INST }, repositories_removed: [{ id: REPO_ID }] });
  await assert.rejects(webhook(s.deps, "whsec-test", "installation_repositories", "sha256=" + "0".repeat(64), removed), (e: ShipError) => e.status === 401);
  assert.deepEqual(await webhook(s.deps, "whsec-test", "installation_repositories", sign(removed), removed), { handled: true, links_removed: 1 });
  await assert.rejects(ciSession(s.deps, `Bearer ${await oidc()}`, T0), (e: ShipError) => e.status === 404);
  const again = await startLink(s.deps, s.org, { repo: FULL, app: "shop" }, T0);
  assert.ok("done" in (await go(s.deps, again.id, T0)), "relinking needs no click once the App and the grant exist");
  const deleted = JSON.stringify({ action: "deleted", installation: { id: INST } });
  assert.deepEqual(await webhook(s.deps, "whsec-test", "installation", sign(deleted), deleted), { handled: true, links_removed: 1 });
  assert.equal(s.store.installations.size, 0);
  const res = await webhookH(s.deps, "whsec-test", new Request(`${CONSOLE}/api/v1/github/webhook`, { method: "POST", headers: { "x-github-event": "ping", "x-hub-signature-256": sign("not json") }, body: "not json" }));
  assert.equal(res.status, 400);
});

// ----------------------------------------------------------- no tokens ----

test("no token is ever shown: pages, link responses and status carry none; the workflow file holds no secret", async () => {
  const s = await setup();
  const token = `ry_${"t".repeat(30)}`;
  await s.repo.createToken({ org_id: s.org, user_id: "u1", token_hash: await (await import("../crypto.ts")).sha256Hex(token), label: "cli" });
  const res = await linkStartH(s.deps, s.repo, new Request(`${CONSOLE}/api/v1/github/link`, { method: "POST", headers: { authorization: `Bearer ${token}` }, body: JSON.stringify({ repo: FULL, app: "shop" }) }), T0);
  assert.equal(res.status, 201);
  const started = (await res.json()) as { id: string; url: string };
  assert.ok(started.url.startsWith(`${CONSOLE}/api/v1/github/link/go?s=`));
  assert.equal((await linkStartH(s.deps, s.repo, new Request(`${CONSOLE}/x`, { method: "POST" }), T0)).status, 401);
  assert.equal((await linkStartH(null, s.repo, new Request(`${CONSOLE}/x`, { method: "POST", headers: { authorization: `Bearer ${token}` } }), T0)).status, 503);
  const goRes = await goH(s.deps, new Request(started.url), T0);
  assert.equal(goRes.status, 302);
  s.gh.installedOn.add(FULL);
  const { done } = await linkThrough(s);
  const html = await page("Linked", done.done).text();
  for (const secret of ["cf-access", "cf-refresh", "ghs_install", "ry_", APP_PEM.slice(40, 80)]) assert.ok(!html.includes(secret), secret);
  assert.match(html, /<a href="https:\/\/github\.com\/acme\/shop\/pull\/\d+">/);
  const wf = workflowYaml({ consoleUrl: CONSOLE, productionBranch: "main" });
  assert.match(wf, /id-token: write/);
  assert.match(wf, /contents: read/);
  assert.ok(!/secrets\.|CLOUDFLARE_API_TOKEN|(?<!id-)token:/i.test(wf), "no secret in the workflow");
  assert.throws(() => workflowYaml({ consoleUrl: "http://insecure.example", productionBranch: "main" }));
  // The session answer (the job's only secret) goes to the job, never to a page.
  const sess = await ciSessionH(s.deps, new Request(`${CONSOLE}/x`, { method: "POST", headers: { authorization: `Bearer ${await oidc()}` } }), T0);
  assert.equal(sess.status, 200);
  assert.equal(sess.headers.get("cache-control"), "no-store");
});

test("the GitHub App manifest asks only for what ship on push uses; the done page never shows the key", async () => {
  const m = manifest(CONSOLE);
  assert.deepEqual(m.default_permissions, { contents: "write", workflows: "write", pull_requests: "write", metadata: "read" });
  assert.equal(m.hook_attributes.url, `${CONSOLE}/api/v1/github/webhook`);
  assert.equal(m.setup_url, `${CONSOLE}/api/v1/github/setup`);
  const done = await manifestDone("abc123def456").text();
  assert.match(done, /app-manifests\/abc123def456\/conversions/);
  assert.match(done, /wrangler secret put GITHUB_APP_PRIVATE_KEY/);
  assert.equal(manifestDone("<script>").status, 400);
});

test("config: ship on push stays off until RAILYARD_SHIP_ON_PUSH_LIVE=1 and every name is set", async () => {
  const { readConfig } = await import("../config.ts");
  const base = {
    SCRIBASE_URL: "https://sb.example", SCRIBASE_ANON_KEY: "a", SCRIBASE_SERVICE_ROLE_KEY: "s", RAILYARD_SESSION_SECRET: "x", DODO_API_KEY: "d", DODO_WEBHOOK_SECRET: "w",
    DODO_PRODUCT_ID_CARD_CHECK: "p1", DODO_PRODUCT_ID_MONTH: "p2", DODO_PRODUCT_ID_YEAR: "p3",
    GITHUB_APP_ID: "12345", GITHUB_APP_SLUG: "railyard", GITHUB_APP_PRIVATE_KEY: "pem", GITHUB_APP_WEBHOOK_SECRET: "wh",
    CLOUDFLARE_OAUTH_CLIENT_ID: "cid", CLOUDFLARE_OAUTH_CLIENT_SECRET: "cs", RAILYARD_CREDENTIALS_KEY: CREDS_KEY,
  };
  assert.equal(readConfig(base).ship, null, "flag off");
  const on = readConfig({ ...base, RAILYARD_SHIP_ON_PUSH_LIVE: "1", CLOUDFLARE_OAUTH_SCOPES: "workers:write, offline_access" });
  assert.deepEqual(on.ship?.cloudflare.scopes, ["workers:write", "offline_access"]);
  assert.equal(on.ship?.github.slug, "railyard");
  assert.equal(readConfig({ ...base, RAILYARD_SHIP_ON_PUSH_LIVE: "1", CLOUDFLARE_OAUTH_CLIENT_SECRET: "" }).ship, null, "a missing name keeps it off");
  assert.throws(() => readConfig({ ...base, RAILYARD_SHIP_ON_PUSH_LIVE: "1", GITHUB_APP_ID: "abc" }));
});
