// Ship on push (Wave P PY3.1, RYP-E3, ADR RY-0004 path A).
//
//   railyard link  -> startLink: a 15-minute browser session for (org, app, repo)
//   browser        -> go: install the GitHub App on the repo (skipped when it already is),
//                     then "Connect Cloudflare" (skipped when the org already has a grant),
//                     then finish: the link row and one pull request adding the workflow
//   the workflow   -> ciSession: GitHub OIDC token in, short-lived Cloudflare token out
//                  -> ciReport: the deploy record, and the preview URL on the branch's PR
//   GitHub         -> webhook: an uninstall or a removed repository drops its links
//
// At no step does anyone see or paste a token: the grant is Cloudflare's consent
// screen, the job's identity is GitHub's OIDC token, the CLI masks what it gets.

import { normaliseAppName, pausedMessage } from "../apps.ts";
import { hmacSha256, hex, randomToken, sha256Hex, constantTimeEqual } from "../crypto.ts";
import { access } from "../ladder.ts";
import type { Repo } from "../repo.ts";
import { open, seal } from "../ai/seal.ts";
import { recordDeploy, type DeployInput } from "../../app/api/v1/deploys/service.ts";
import type { SitesStore } from "../sites.ts";
import type { CloudflareOAuth } from "./cloudflare.ts";
import { GithubError, type GithubApp } from "./github.ts";
import { verifyRs256, type Jwk } from "./jwt.ts";
import type { ShipStore } from "./store.ts";
import type { CloudflareConnection, LinkSession, ShipLink } from "./types.ts";
import { WORKFLOW_PATH, workflowYaml } from "./workflow.ts";

export const SESSION_TTL_SECONDS = 15 * 60;
export const GITHUB_OIDC_ISSUER = "https://token.actions.githubusercontent.com";
/** A cached access token is reused while it has at least this long left. */
const ACCESS_REUSE_SECONDS = 10 * 60;
const REPO = /^[A-Za-z0-9][A-Za-z0-9-]{0,38}\/[A-Za-z0-9._-]{1,100}$/;
const FRAMEWORKS = ["nextjs", "vite", "astro", "static"] as const;

export interface ShipDeps {
  store: ShipStore;
  repo: Repo;
  github: GithubApp;
  cloudflare: CloudflareOAuth;
  /** GitHub's Actions OIDC signing keys (cached by the caller). */
  oidcKeys: () => Promise<Jwk[]>;
  consoleUrl: string;
  credentialsKey: string;
  sites?: SitesStore;
}

export class ShipError extends Error {
  status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

const sealAad = (orgId: string, what: "refresh" | "access") => `railyard:cloudflare:${what}:${orgId}`;

// ---------------------------------------------------------------- link ----

export async function startLink(deps: ShipDeps, orgId: string, body: unknown, now: Date): Promise<{ id: string; url: string; expires_at: string }> {
  const b = (body ?? {}) as Record<string, unknown>;
  const repoName = typeof b.repo === "string" ? b.repo.trim().replace(/\.git$/, "") : "";
  if (!REPO.test(repoName)) throw new ShipError("repo must look like owner/name", 400);
  const app = normaliseAppName(typeof b.app === "string" ? b.app : "");
  if (!/^[a-z0-9][a-z0-9-]{0,39}$/.test(app)) throw new ShipError("app must use letters, numbers and dashes", 400);
  const paused = pausedMessage(access(await deps.repo.getEntitlement(orgId), now));
  if (paused) throw new ShipError(paused, 402);
  const id = randomToken(24);
  const expires = new Date(now.getTime() + SESSION_TTL_SECONDS * 1000).toISOString();
  await deps.store.createSession({
    id_hash: await sha256Hex(id),
    org_id: orgId,
    app,
    repo_full_name: repoName,
    installation_id: null,
    pkce_verifier: randomToken(48),
    state: "started",
    message: null,
    link_id: null,
    expires_at: expires,
    created_at: now.toISOString(),
  });
  return { id, url: `${deps.consoleUrl}/api/v1/github/link/go?s=${encodeURIComponent(id)}`, expires_at: expires };
}

async function liveSession(deps: ShipDeps, id: string | null, now: Date): Promise<LinkSession> {
  if (!id || id.length < 20 || id.length > 100) throw new ShipError("This link is not valid. Run railyard link again.", 400);
  const s = await deps.store.getSession(await sha256Hex(id));
  if (!s) throw new ShipError("This link is not valid. Run railyard link again.", 404);
  if (Date.parse(s.expires_at) <= now.getTime()) throw new ShipError("This link expired. Run railyard link again.", 410);
  if (s.state === "linked") throw new ShipError("This repository is already linked; your next push goes live.", 409);
  return s;
}

/** The first browser step: where to send the user next. */
export async function go(deps: ShipDeps, id: string | null, now: Date): Promise<{ redirect: string } | { done: string }> {
  const s = await liveSession(deps, id, now);
  const inst = await deps.github.repoInstallation(s.repo_full_name, now);
  if (!inst) return { redirect: deps.github.installUrl(id as string) };
  return afterGithub(deps, id as string, s, inst, now);
}

async function afterGithub(deps: ShipDeps, id: string, s: LinkSession, inst: { id: number; account_login: string }, now: Date) {
  // The first org to link through an installation is recorded; links carry
  // their own installation id, so a shared GitHub organization still works.
  const known = await deps.store.getInstallation(inst.id);
  if (!known) await deps.store.saveInstallation({ installation_id: inst.id, org_id: s.org_id, account_login: inst.account_login, created_at: now.toISOString() });
  await deps.store.updateSession(await sha256Hex(id), { installation_id: inst.id, state: "github_done" });
  if (await deps.store.getCloudflare(s.org_id)) return { done: await finish(deps, id, { ...s, installation_id: inst.id }, now) };
  return { redirect: await deps.cloudflare.authorizeLink(id, s.pkce_verifier) };
}

/**
 * GitHub's Setup URL after an install. The installation id in the query is not
 * trusted: the App's own view of the repository decides which installation it is.
 */
export async function githubSetup(deps: ShipDeps, q: URLSearchParams, now: Date): Promise<{ redirect: string } | { done: string }> {
  const id = q.get("state");
  const s = await liveSession(deps, id, now);
  const inst = await deps.github.repoInstallation(s.repo_full_name, now);
  if (!inst) throw new ShipError(`The Railyard app was installed, but not on ${s.repo_full_name}. Add that repository to the installation and open the link again.`, 409);
  return afterGithub(deps, id as string, s, inst, now);
}

/** Cloudflare's redirect after the consent screen. */
export async function cloudflareCallback(deps: ShipDeps, q: URLSearchParams, now: Date): Promise<{ done: string }> {
  const id = q.get("state");
  const s = await liveSession(deps, id, now);
  if (q.get("error")) {
    await deps.store.updateSession(await sha256Hex(id as string), { state: "failed", message: "Cloudflare access was not granted." });
    throw new ShipError("Cloudflare access was not granted, so nothing was linked. Run railyard link again when you are ready.", 400);
  }
  if (s.state !== "github_done" || !s.installation_id) throw new ShipError("Install the Railyard app on GitHub first: open the link from railyard link again.", 409);
  const code = q.get("code");
  if (!code || code.length > 2000) throw new ShipError("Cloudflare sent no code. Run railyard link again.", 400);
  const tokens = await deps.cloudflare.exchange(code, s.pkce_verifier);
  if (!tokens.refresh_token) throw new ShipError("Cloudflare gave no lasting access (the offline_access scope is missing on the OAuth client).", 502);
  const accounts = await deps.cloudflare.accounts(tokens.access_token);
  if (!accounts[0]) throw new ShipError("No Cloudflare account was granted. Pick one account on the Cloudflare screen.", 400);
  const account = accounts[0];
  const refresh = await seal(deps.credentialsKey, sealAad(s.org_id, "refresh"), tokens.refresh_token);
  const acc = await seal(deps.credentialsKey, sealAad(s.org_id, "access"), tokens.access_token);
  await deps.store.saveCloudflare({
    org_id: s.org_id,
    account_id: account.id,
    account_name: account.name,
    refresh_nonce: refresh.nonce,
    refresh_ciphertext: refresh.ciphertext,
    access_nonce: acc.nonce,
    access_ciphertext: acc.ciphertext,
    access_expires_at: new Date(now.getTime() + tokens.expires_in * 1000).toISOString(),
    updated_at: now.toISOString(),
  });
  return { done: await finish(deps, id as string, s, now) };
}

async function finish(deps: ShipDeps, id: string, s: LinkSession, now: Date): Promise<string> {
  const idHash = await sha256Hex(id);
  try {
    const token = await deps.github.installationToken(s.installation_id as number, now);
    const repo = await deps.github.repo(token, s.repo_full_name);
    const other = await deps.store.findLinkByRepoId(repo.id);
    if (other && other.org_id !== s.org_id) throw new ShipError("This repository is linked to another Railyard organization.", 403);
    const prUrl = await deps.github.openWorkflowPr(token, repo, WORKFLOW_PATH, workflowYaml({ consoleUrl: deps.consoleUrl, productionBranch: repo.default_branch }));
    const link = await deps.store.saveLink({
      org_id: s.org_id,
      app: s.app,
      repo_id: repo.id,
      repo_full_name: repo.full_name,
      installation_id: s.installation_id as number,
      production_branch: repo.default_branch,
      workflow_pr_url: prUrl,
    });
    await deps.store.updateSession(idHash, { state: "linked", link_id: link.id, message: null });
    return prUrl
      ? `Linked ${repo.full_name} to ${s.app}. Merge the pull request (${prUrl}); after that every push deploys.`
      : `Linked ${repo.full_name} to ${s.app}. The workflow is already on ${repo.default_branch}; your next push there goes live.`;
  } catch (e) {
    const message = e instanceof ShipError || e instanceof GithubError ? e.message : "Railyard could not finish the link.";
    await deps.store.updateSession(idHash, { state: "failed", message });
    throw e instanceof ShipError ? e : new ShipError(message, 502);
  }
}

/** What `railyard link` polls: the session's state, only for the org that started it. */
export async function linkStatus(deps: ShipDeps, orgId: string, id: string | null) {
  if (!id) throw new ShipError("id is required", 400);
  const s = await deps.store.getSession(await sha256Hex(id));
  if (!s || s.org_id !== orgId) throw new ShipError("unknown link", 404);
  const link = s.link_id ? await deps.store.getLink(orgId, s.link_id) : null;
  return { state: s.state, message: s.message, repo: s.repo_full_name, app: s.app, pr_url: link?.workflow_pr_url ?? null, production_branch: link?.production_branch ?? null };
}

// ------------------------------------------------------------------ ci ----

export interface CiIdentity {
  link: ShipLink;
  branch: string;
  sha: string;
  production: boolean;
}

/** Checks the job's GitHub OIDC token and finds the linked repository it speaks for. */
export async function ciIdentity(deps: ShipDeps, authorization: string | null, now: Date): Promise<CiIdentity> {
  const m = /^Bearer\s+([A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+)$/.exec(authorization ?? "");
  if (!m) throw new ShipError("unauthorized", 401);
  const v = await verifyRs256(m[1] as string, await deps.oidcKeys(), { issuer: GITHUB_OIDC_ISSUER, audience: deps.consoleUrl, now });
  if (!v.ok) throw new ShipError(`unauthorized: ${v.error}`, 401);
  const c = v.claims;
  const repoId = Number(c.repository_id);
  if (!Number.isSafeInteger(repoId) || repoId <= 0) throw new ShipError("unauthorized: no repository", 401);
  const link = await deps.store.findLinkByRepoId(repoId);
  if (!link) throw new ShipError("This repository is not linked. Run railyard link in it.", 404);
  // Only the workflow Railyard added may ask; another workflow in the repo cannot.
  const wf = String(c.job_workflow_ref ?? c.workflow_ref ?? "");
  if (!wf.startsWith(`${c.repository}/${WORKFLOW_PATH}@`)) throw new ShipError("Only the Railyard workflow may deploy.", 403);
  if (!["push", "workflow_dispatch"].includes(String(c.event_name))) throw new ShipError("Deploys run on push only.", 403);
  const ref = String(c.ref ?? "");
  if (!ref.startsWith("refs/heads/")) throw new ShipError("Deploys run for branches only.", 403);
  const branch = ref.slice("refs/heads/".length);
  const sha = String(c.sha ?? "");
  if (!/^[0-9a-f]{40}$/.test(sha)) throw new ShipError("unauthorized: no commit", 401);
  return { link, branch, sha, production: branch === link.production_branch };
}

async function cloudflareAccess(deps: ShipDeps, conn: CloudflareConnection, now: Date): Promise<{ token: string; expiresIn: number }> {
  if (conn.access_nonce && conn.access_ciphertext && conn.access_expires_at) {
    const left = Math.floor((Date.parse(conn.access_expires_at) - now.getTime()) / 1000);
    if (left >= ACCESS_REUSE_SECONDS) return { token: await open(deps.credentialsKey, sealAad(conn.org_id, "access"), conn.access_nonce, conn.access_ciphertext), expiresIn: left };
  }
  const refresh = await open(deps.credentialsKey, sealAad(conn.org_id, "refresh"), conn.refresh_nonce, conn.refresh_ciphertext);
  const t = await deps.cloudflare.refresh(refresh);
  const acc = await seal(deps.credentialsKey, sealAad(conn.org_id, "access"), t.access_token);
  const next = t.refresh_token ? await seal(deps.credentialsKey, sealAad(conn.org_id, "refresh"), t.refresh_token) : { nonce: conn.refresh_nonce, ciphertext: conn.refresh_ciphertext };
  await deps.store.saveCloudflare({
    ...conn,
    refresh_nonce: next.nonce,
    refresh_ciphertext: next.ciphertext,
    access_nonce: acc.nonce,
    access_ciphertext: acc.ciphertext,
    access_expires_at: new Date(now.getTime() + t.expires_in * 1000).toISOString(),
    updated_at: now.toISOString(),
  });
  return { token: t.access_token, expiresIn: t.expires_in };
}

/** POST /api/v1/github/ci/session: what the job needs to deploy, nothing more. */
export async function ciSession(deps: ShipDeps, authorization: string | null, now: Date) {
  const who = await ciIdentity(deps, authorization, now);
  const paused = pausedMessage(access(await deps.repo.getEntitlement(who.link.org_id), now));
  if (paused) throw new ShipError(paused, 402);
  const conn = await deps.store.getCloudflare(who.link.org_id);
  if (!conn) throw new ShipError("Cloudflare is not connected for this organization. Run railyard link again.", 409);
  const cf = await cloudflareAccess(deps, conn, now);
  return {
    app: who.link.app,
    branch: who.branch,
    production: who.production,
    production_branch: who.link.production_branch,
    cloudflare: { account_id: conn.account_id, access_token: cf.token, expires_in: cf.expiresIn },
  };
}

/**
 * POST /api/v1/github/ci/report: the job's result. The branch and commit come
 * from the verified token, not the body. A preview goes on the branch's PR.
 */
export async function ciReport(deps: ShipDeps, authorization: string | null, body: unknown, now: Date) {
  const who = await ciIdentity(deps, authorization, now);
  const b = (body ?? {}) as Record<string, unknown>;
  const status = b.status === "succeeded" ? "succeeded" : b.status === "failed" ? "failed" : null;
  if (!status) throw new ShipError("status must be succeeded or failed", 400);
  const url = typeof b.url === "string" && /^https:\/\/[a-z0-9.-]+\.workers\.dev(\/|$)/i.test(b.url) ? b.url : "";
  if (status === "succeeded" && !url) throw new ShipError("a successful deploy needs its workers.dev URL", 400);
  const framework = FRAMEWORKS.includes(b.framework as (typeof FRAMEWORKS)[number]) ? (b.framework as DeployInput["framework"]) : "static";
  const input: DeployInput = {
    app: who.link.app,
    url,
    commit_sha: who.sha,
    branch: who.branch,
    framework,
    version_id: typeof b.version_id === "string" && /^[0-9a-f-]{8,64}$/i.test(b.version_id) ? b.version_id : null,
    duration_ms: typeof b.duration_ms === "number" && Number.isInteger(b.duration_ms) && b.duration_ms >= 0 ? b.duration_ms : null,
    status,
    rollback_of: null,
  };
  const recorded = await recordDeploy(deps.repo, who.link.org_id, input, now, deps.sites);
  if (recorded.status >= 400) throw new ShipError(String(recorded.body.error ?? "the deploy was not recorded"), recorded.status);
  let comment: string | null = null;
  if (!who.production) {
    const token = await deps.github.installationToken(who.link.installation_id, now);
    const repo = await deps.github.repo(token, who.link.repo_full_name);
    const pr = await deps.github.openPrForBranch(token, repo, who.branch);
    if (pr) {
      const line = status === "succeeded" ? `Preview of \`${who.branch}\` at ${who.sha.slice(0, 7)}: ${url}` : `The preview of \`${who.branch}\` at ${who.sha.slice(0, 7)} failed to deploy. The workflow log says why.`;
      comment = await deps.github.upsertPreviewComment(token, repo.full_name, pr, `${line}\n\nDeployed by Railyard to your Cloudflare account.`);
    }
  }
  return { deploy: recorded.body.deploy, comment_url: comment };
}

// ------------------------------------------------------------- webhook ----

export async function verifyGithubSignature(secret: string, raw: string, header: string | null): Promise<boolean> {
  const m = /^sha256=([0-9a-f]{64})$/.exec(header ?? "");
  if (!m) return false;
  return constantTimeEqual(hex(await hmacSha256(secret, raw)), m[1] as string);
}

/** Installation removed: its links go. Repositories removed from it: their links go. */
export async function webhook(deps: ShipDeps, secret: string, event: string | null, signature: string | null, raw: string) {
  if (!(await verifyGithubSignature(secret, raw, signature))) throw new ShipError("bad signature", 401);
  const body = JSON.parse(raw) as { action?: string; installation?: { id?: number }; repositories_removed?: { id: number }[] };
  const inst = Number(body.installation?.id);
  if (!Number.isSafeInteger(inst)) return { handled: false };
  if (event === "installation" && body.action === "deleted") {
    const removed = await deps.store.deleteLinksByInstallation(inst);
    await deps.store.deleteInstallation(inst);
    return { handled: true, links_removed: removed };
  }
  if (event === "installation_repositories" && body.action === "removed") {
    const ids = (body.repositories_removed ?? []).map((r) => Number(r.id)).filter((n) => Number.isSafeInteger(n));
    return { handled: true, links_removed: await deps.store.deleteLinksByRepoIds(inst, ids) };
  }
  return { handled: false };
}
