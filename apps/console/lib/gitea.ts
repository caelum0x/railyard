// Rithub (apps/gitea) self-serve provisioning (backlog item 4): turns a
// signed-in Console session straight into a dedicated Gitea user, a private
// org and a first repo -- no separate native Gitea signup form. Server-only:
// the admin token never reaches a client component.
//
// Uses Gitea's documented `sudo=<username>` query parameter: a site-admin
// token can act "as" any user, so the org and repo end up correctly owned by
// the newly created user instead of the admin account. Verified live against
// a real gitea/gitea:1.23 container (create user -> create org as sudo ->
// create repo in org as sudo; duplicate calls answer 422/409 as expected).

export interface GiteaProvision {
  username: string;
  orgSlug: string;
  repoName: string;
  htmlUrl: string;
  cloneUrl: string;
}

export interface GiteaClient {
  /** Idempotent at the caller's level only: always provisions a *new* Gitea user. Callers must cache the result (see Org.gitea_username) and skip calling this again for an org that already has one. */
  provision(input: { seed: string; email: string; repoName: string }): Promise<GiteaProvision>;
}

const MAX_SLUG_ATTEMPTS = 5;

/** Mirrors switchyard-server's signup.rs slugify (lower-case alnum, single dashes, no leading/trailing dash). */
export function slugify(raw: string): string {
  let out = "";
  let lastDash = true; // suppress a leading dash
  for (const ch of raw.trim().toLowerCase()) {
    if (ch >= "a" && ch <= "z" || ch >= "0" && ch <= "9") {
      out += ch;
      lastDash = false;
    } else if (!lastDash) {
      out += "-";
      lastDash = true;
    }
  }
  const base = out.replace(/-+$/, "").slice(0, 32);
  return base || "org";
}

function randomSuffix(): string {
  return Math.random().toString(36).slice(2, 8);
}

function randomPassword(): string {
  // Gitea requires a password; the account signs in via OIDC SSO (backlog
  // item 5) or an admin-reset, never this generated secret directly.
  return `${crypto.randomUUID().replace(/-/g, "")}Aa1!`;
}

interface GiteaRepoBody {
  html_url: string;
  clone_url: string;
}

class HttpGiteaClient implements GiteaClient {
  constructor(private base: string, private adminToken: string, private fetchImpl: typeof fetch = fetch) {}

  private async req(path: string, init?: RequestInit): Promise<Response> {
    return this.fetchImpl(`${this.base}${path}`, {
      ...init,
      headers: { "content-type": "application/json", authorization: `token ${this.adminToken}`, ...(init?.headers ?? {}) },
    });
  }

  private async createUser(base: string, email: string): Promise<string> {
    for (let attempt = 0; attempt < MAX_SLUG_ATTEMPTS; attempt++) {
      const username = `${base}-${randomSuffix()}`;
      const res = await this.req("/admin/users", {
        method: "POST",
        body: JSON.stringify({ username, email, password: randomPassword(), must_change_password: false }),
      });
      if (res.status === 201) return username;
      if (res.status !== 422) throw new Error(`gitea create user failed with ${res.status}`);
      // 422: username or email already taken. A fresh random suffix clears a
      // username collision; an email collision (the same person retrying) is
      // rare under the caller's own idempotency check and surfaces here as
      // repeated 422s, correctly failing closed rather than silently reusing
      // someone else's account.
    }
    throw new Error("gitea: could not allocate a free username after several attempts");
  }

  private async createOrg(username: string, base: string): Promise<string> {
    for (let attempt = 0; attempt < MAX_SLUG_ATTEMPTS; attempt++) {
      const orgSlug = attempt === 0 ? base : `${base}-${randomSuffix()}`;
      const res = await this.req(`/orgs?sudo=${encodeURIComponent(username)}`, {
        method: "POST",
        body: JSON.stringify({ username: orgSlug, visibility: "private" }),
      });
      if (res.status === 201) return orgSlug;
      if (res.status !== 422) throw new Error(`gitea create org failed with ${res.status}`);
    }
    throw new Error("gitea: could not allocate a free org name after several attempts");
  }

  private async createRepo(username: string, orgSlug: string, repoName: string): Promise<GiteaRepoBody> {
    const res = await this.req(`/orgs/${encodeURIComponent(orgSlug)}/repos?sudo=${encodeURIComponent(username)}`, {
      method: "POST",
      body: JSON.stringify({ name: repoName, auto_init: true, default_branch: "main" }),
    });
    if (res.status === 201) return (await res.json()) as GiteaRepoBody;
    if (res.status === 409) {
      const existing = await this.req(`/repos/${encodeURIComponent(orgSlug)}/${encodeURIComponent(repoName)}`);
      if (!existing.ok) throw new Error(`gitea repo lookup failed with ${existing.status}`);
      return (await existing.json()) as GiteaRepoBody;
    }
    throw new Error(`gitea create repo failed with ${res.status}`);
  }

  async provision(input: { seed: string; email: string; repoName: string }): Promise<GiteaProvision> {
    const base = slugify(input.seed);
    const username = await this.createUser(base, input.email);
    const orgSlug = await this.createOrg(username, base);
    const repo = await this.createRepo(username, orgSlug, input.repoName);
    return { username, orgSlug, repoName: input.repoName, htmlUrl: repo.html_url, cloneUrl: repo.clone_url };
  }
}

export function giteaClient(opts: { apiUrl: string; adminToken: string; fetchImpl?: typeof fetch }): GiteaClient {
  return new HttpGiteaClient(opts.apiUrl.replace(/\/$/, ""), opts.adminToken, opts.fetchImpl);
}
