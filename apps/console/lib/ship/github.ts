// The Railyard GitHub App (PY3.1, owner item Y4 creates it from the manifest).
// It needs: contents write and pull requests write (to open the one workflow
// pull request and to comment preview URLs), metadata read. It never pushes to
// a default branch: the workflow arrives as a pull request the user merges.

import { importRsaPrivateKey, signRs256 } from "./jwt.ts";

type Fetch = typeof fetch;

export class GithubError extends Error {
  status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

export interface GithubAppConfig {
  appId: string;
  slug: string;
  privateKey: string;
  webhookSecret: string;
  /** The App's own OAuth client (user sign-in): proves the person linking can push to the repository. */
  clientId?: string;
  clientSecret?: string;
  apiBase?: string;
  /** github.com (sign-in pages and the code exchange). */
  webBase?: string;
  fetchImpl?: Fetch;
}

export interface RepoInfo {
  id: number;
  full_name: string;
  default_branch: string;
  owner_login: string;
}

export const WORKFLOW_BRANCH = "railyard/ship-on-push";
const COMMENT_MARKER = "<!-- railyard-preview -->";

const b64 = (s: string) => btoa(String.fromCharCode(...new TextEncoder().encode(s)));

export class GithubApp {
  cfg: GithubAppConfig;
  api: string;
  fetchImpl: Fetch;
  private key: Promise<CryptoKey> | null = null;

  constructor(cfg: GithubAppConfig) {
    this.cfg = cfg;
    this.api = (cfg.apiBase ?? "https://api.github.com").replace(/\/$/, "");
    this.fetchImpl = cfg.fetchImpl ?? fetch;
  }

  private get web(): string {
    return (this.cfg.webBase ?? "https://github.com").replace(/\/$/, "");
  }

  /** GitHub's sign-in for the App (user authorization). No scope: a GitHub App's user token carries the App's permissions. */
  userAuthorizeUrl(state: string, redirectUri: string): string {
    if (!this.cfg.clientId) throw new GithubError("The GitHub App's client id is not configured", 503);
    const q = new URLSearchParams({ client_id: this.cfg.clientId, redirect_uri: redirectUri, state, allow_signup: "false" });
    return `${this.web}/login/oauth/authorize?${q}`;
  }

  /** Trades the sign-in code for the user's token (used once, then dropped; never stored). */
  async userToken(code: string, redirectUri: string): Promise<string> {
    if (!this.cfg.clientId || !this.cfg.clientSecret) throw new GithubError("The GitHub App's client is not configured", 503);
    const res = await this.fetchImpl(`${this.web}/login/oauth/access_token`, {
      method: "POST",
      headers: { accept: "application/json", "content-type": "application/json", "user-agent": "railyard-console" },
      body: JSON.stringify({ client_id: this.cfg.clientId, client_secret: this.cfg.clientSecret, code, redirect_uri: redirectUri }),
    });
    const body = (await res.json().catch(() => ({}))) as { access_token?: unknown; error?: unknown };
    if (!res.ok || typeof body.access_token !== "string" || !body.access_token) {
      throw new GithubError("GitHub did not confirm the sign-in. Run railyard link again.", res.status >= 500 ? 502 : 400);
    }
    return body.access_token;
  }

  /**
   * What the signed-in user may do on the repository, seen with their own token
   * (so it is their permission, limited to where the App is installed). Null
   * when they cannot see it at all.
   */
  async userRepoAccess(userToken: string, fullName: string): Promise<{ id: number; full_name: string; push: boolean } | null> {
    const r = await this.call<{ id: number; full_name: string; permissions?: { admin?: boolean; maintain?: boolean; push?: boolean } }>("GET", `/repos/${fullName}`, userToken, undefined, true);
    if (!r || !Number.isSafeInteger(r.id)) return null;
    const p = r.permissions ?? {};
    return { id: r.id, full_name: r.full_name, push: p.admin === true || p.maintain === true || p.push === true };
  }

  installUrl(state: string): string {
    return `https://github.com/apps/${encodeURIComponent(this.cfg.slug)}/installations/new?state=${encodeURIComponent(state)}`;
  }

  /** The App's own JWT (10 minutes; `iat` 60 s back for clock drift, as GitHub advises). */
  async appJwt(now: Date): Promise<string> {
    this.key ??= importRsaPrivateKey(this.cfg.privateKey);
    const iat = Math.floor(now.getTime() / 1000) - 60;
    return signRs256(await this.key, { iat, exp: iat + 600, iss: this.cfg.appId });
  }

  private async call<T>(method: string, path: string, token: string, body?: unknown, okMissing = false): Promise<T | null> {
    const res = await this.fetchImpl(`${this.api}${path}`, {
      method,
      headers: {
        accept: "application/vnd.github+json",
        authorization: `Bearer ${token}`,
        "x-github-api-version": "2022-11-28",
        "user-agent": "railyard-console",
        ...(body === undefined ? {} : { "content-type": "application/json" }),
      },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (okMissing && res.status === 404) return null;
    if (!res.ok) throw new GithubError(`GitHub ${method} ${path.split("?")[0]} answered ${res.status}`, res.status);
    const text = await res.text();
    return (text ? JSON.parse(text) : {}) as T;
  }

  /** The installation that covers the repository, or null when the App is not installed there. */
  async repoInstallation(fullName: string, now: Date): Promise<{ id: number; account_login: string } | null> {
    const r = await this.call<{ id: number; account: { login: string } }>("GET", `/repos/${fullName}/installation`, await this.appJwt(now), undefined, true);
    return r ? { id: r.id, account_login: r.account.login } : null;
  }

  async installationToken(installationId: number, now: Date): Promise<string> {
    const r = await this.call<{ token: string }>("POST", `/app/installations/${installationId}/access_tokens`, await this.appJwt(now), {});
    if (!r?.token) throw new GithubError("GitHub gave no installation token", 502);
    return r.token;
  }

  async repo(token: string, fullName: string): Promise<RepoInfo> {
    return this.repoFrom(await this.call<{ id: number; full_name: string; default_branch: string; owner: { login: string } }>("GET", `/repos/${fullName}`, token));
  }

  /** The repository by the id stored at the push check (P-Y11), not by its current name. */
  async repoById(token: string, repoId: number): Promise<RepoInfo> {
    if (!Number.isSafeInteger(repoId) || repoId <= 0) throw new GithubError("repository not found", 404);
    return this.repoFrom(await this.call<{ id: number; full_name: string; default_branch: string; owner: { login: string } }>("GET", `/repositories/${repoId}`, token));
  }

  private repoFrom(r: { id: number; full_name: string; default_branch: string; owner: { login: string } } | null): RepoInfo {
    if (!r || !Number.isSafeInteger(r.id)) throw new GithubError("repository not found", 404);
    return { id: r.id, full_name: r.full_name, default_branch: r.default_branch, owner_login: r.owner.login };
  }

  /**
   * Opens (or finds) the pull request that adds the workflow file. Returns its
   * URL, or null when the default branch already carries the same file.
   */
  async openWorkflowPr(token: string, repo: RepoInfo, path: string, content: string): Promise<string | null> {
    const onDefault = await this.call<{ content: string; sha: string }>("GET", `/repos/${repo.full_name}/contents/${path}?ref=${encodeURIComponent(repo.default_branch)}`, token, undefined, true);
    if (onDefault && atob(onDefault.content.replace(/\s+/g, "")) === content) return null;
    const open = await this.call<{ html_url: string }[]>("GET", `/repos/${repo.full_name}/pulls?state=open&head=${encodeURIComponent(`${repo.owner_login}:${WORKFLOW_BRANCH}`)}`, token);
    if (open?.[0]) return open[0].html_url;
    const head = await this.call<{ object: { sha: string } }>("GET", `/repos/${repo.full_name}/git/ref/heads/${encodeURIComponent(repo.default_branch)}`, token);
    if (!head) throw new GithubError("default branch not found", 404);
    const existingBranch = await this.call<{ object: { sha: string } }>("GET", `/repos/${repo.full_name}/git/ref/heads/${WORKFLOW_BRANCH}`, token, undefined, true);
    if (existingBranch) {
      await this.call("PATCH", `/repos/${repo.full_name}/git/refs/heads/${WORKFLOW_BRANCH}`, token, { sha: head.object.sha, force: true });
    } else {
      await this.call("POST", `/repos/${repo.full_name}/git/refs`, token, { ref: `refs/heads/${WORKFLOW_BRANCH}`, sha: head.object.sha });
    }
    const onBranch = await this.call<{ sha: string }>("GET", `/repos/${repo.full_name}/contents/${path}?ref=${WORKFLOW_BRANCH}`, token, undefined, true);
    await this.call("PUT", `/repos/${repo.full_name}/contents/${path}`, token, {
      message: "Deploy to Cloudflare on push with Railyard",
      content: b64(content),
      branch: WORKFLOW_BRANCH,
      ...(onBranch ? { sha: onBranch.sha } : {}),
    });
    const pr = await this.call<{ html_url: string }>("POST", `/repos/${repo.full_name}/pulls`, token, {
      title: "Ship to Cloudflare from GitHub with Railyard",
      head: WORKFLOW_BRANCH,
      base: repo.default_branch,
      body: [
        "Merging this adds one workflow file. After that:",
        "",
        `- a push to \`${repo.default_branch}\` deploys to your Cloudflare account;`,
        "- a push to any other branch deploys a preview and posts its URL on the branch's pull request.",
        "",
        "No key is stored in this repository. The job proves which repository it runs for with GitHub's own OIDC token, and Railyard answers with a short-lived Cloudflare token from the access you granted.",
      ].join("\n"),
    });
    if (!pr?.html_url) throw new GithubError("GitHub did not open the pull request", 502);
    return pr.html_url;
  }

  async openPrForBranch(token: string, repo: RepoInfo, branch: string): Promise<number | null> {
    const prs = await this.call<{ number: number }[]>("GET", `/repos/${repo.full_name}/pulls?state=open&head=${encodeURIComponent(`${repo.owner_login}:${branch}`)}`, token);
    return prs?.[0]?.number ?? null;
  }

  /** Creates or edits the one Railyard preview comment on a pull request; returns its URL. */
  async upsertPreviewComment(token: string, fullName: string, pr: number, body: string): Promise<string> {
    const text = `${COMMENT_MARKER}\n${body}`;
    const comments = (await this.call<{ id: number; body: string; html_url: string }[]>("GET", `/repos/${fullName}/issues/${pr}/comments?per_page=100`, token)) ?? [];
    const mine = comments.find((c) => c.body.startsWith(COMMENT_MARKER));
    const r = mine
      ? await this.call<{ html_url: string }>("PATCH", `/repos/${fullName}/issues/comments/${mine.id}`, token, { body: text })
      : await this.call<{ html_url: string }>("POST", `/repos/${fullName}/issues/${pr}/comments`, token, { body: text });
    return r?.html_url ?? "";
  }
}
