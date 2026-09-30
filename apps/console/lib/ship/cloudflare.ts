// "Connect Cloudflare" for ship on push (PY3.1, ADR RY-0004 path A).
// Cloudflare's self-managed OAuth clients (launched 2026-06-03) support the
// authorization code flow; Railyard adds PKCE (S256). The grant is the user's
// own consent screen: they pick the account and see every scope. Railyard
// stores only the sealed refresh token and hands a job a short-lived access
// token, never the refresh token.

import { b64url } from "../crypto.ts";

type Fetch = typeof fetch;

export interface CloudflareOAuthConfig {
  clientId: string;
  clientSecret: string;
  redirectUri: string;
  /** Scope ids as configured on the OAuth client (CLOUDFLARE_OAUTH_SCOPES). */
  scopes: string[];
  authorizeUrl?: string;
  tokenUrl?: string;
  apiBase?: string;
  fetchImpl?: Fetch;
}

export interface TokenSet {
  access_token: string;
  refresh_token: string | null;
  expires_in: number;
}

export class CloudflareError extends Error {
  status: number;
  constructor(message: string, status: number) {
    super(message);
    this.status = status;
  }
}

export const DEFAULT_AUTHORIZE_URL = "https://dash.cloudflare.com/oauth2/auth";
export const DEFAULT_TOKEN_URL = "https://dash.cloudflare.com/oauth2/token";
/** Enough to deploy Workers with static assets and read the account (wrangler's deploy set). */
export const DEFAULT_SCOPES = ["account:read", "user:read", "workers:write", "workers_scripts:write", "workers_routes:write", "offline_access"];

export async function pkceChallenge(verifier: string): Promise<string> {
  return b64url(new Uint8Array(await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier))));
}

export class CloudflareOAuth {
  cfg: CloudflareOAuthConfig;
  fetchImpl: Fetch;

  constructor(cfg: CloudflareOAuthConfig) {
    this.cfg = cfg;
    this.fetchImpl = cfg.fetchImpl ?? fetch;
  }

  async authorizeLink(state: string, verifier: string): Promise<string> {
    const q = new URLSearchParams({
      response_type: "code",
      client_id: this.cfg.clientId,
      redirect_uri: this.cfg.redirectUri,
      scope: this.cfg.scopes.join(" "),
      state,
      code_challenge: await pkceChallenge(verifier),
      code_challenge_method: "S256",
    });
    return `${this.cfg.authorizeUrl ?? DEFAULT_AUTHORIZE_URL}?${q}`;
  }

  private async token(form: Record<string, string>): Promise<TokenSet> {
    const res = await this.fetchImpl(this.cfg.tokenUrl ?? DEFAULT_TOKEN_URL, {
      method: "POST",
      headers: {
        "content-type": "application/x-www-form-urlencoded",
        authorization: `Basic ${btoa(`${this.cfg.clientId}:${this.cfg.clientSecret}`)}`,
      },
      body: new URLSearchParams(form).toString(),
    });
    const body = (await res.json().catch(() => ({}))) as Record<string, unknown>;
    if (!res.ok || typeof body.access_token !== "string") {
      throw new CloudflareError(typeof body.error === "string" ? `Cloudflare refused: ${body.error}` : `Cloudflare answered ${res.status}`, res.status === 400 ? 400 : 502);
    }
    return {
      access_token: body.access_token,
      refresh_token: typeof body.refresh_token === "string" ? body.refresh_token : null,
      expires_in: typeof body.expires_in === "number" ? body.expires_in : 3600,
    };
  }

  exchange(code: string, verifier: string): Promise<TokenSet> {
    return this.token({ grant_type: "authorization_code", code, redirect_uri: this.cfg.redirectUri, code_verifier: verifier });
  }

  refresh(refreshToken: string): Promise<TokenSet> {
    return this.token({ grant_type: "refresh_token", refresh_token: refreshToken });
  }

  /** The accounts the user granted (the consent screen selects them). */
  async accounts(accessToken: string): Promise<{ id: string; name: string }[]> {
    const res = await this.fetchImpl(`${(this.cfg.apiBase ?? "https://api.cloudflare.com/client/v4").replace(/\/$/, "")}/accounts?per_page=50`, {
      headers: { authorization: `Bearer ${accessToken}` },
    });
    const body = (await res.json().catch(() => ({}))) as { result?: { id: string; name: string }[] };
    if (!res.ok || !Array.isArray(body.result)) throw new CloudflareError(`Cloudflare accounts answered ${res.status}`, 502);
    return body.result.filter((a) => /^[0-9a-f]{32}$/.test(a.id)).map((a) => ({ id: a.id, name: String(a.name ?? "").slice(0, 200) }));
  }
}
