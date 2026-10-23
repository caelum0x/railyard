// Console as a minimal OIDC provider (backlog item 5): closes apps/gitea's
// `TODO(railyard-sso)` so Rithub (Gitea, registered as an `openidConnect`
// auth source) logs in with the same Console session instead of a separate
// native account. Authorization code grant only (no implicit/hybrid), RS256
// id_token and a self-contained RS256 access_token (so /oidc/userinfo needs
// no second token store -- it just verifies the access_token the same way
// the id_token is verified).

import { randomToken, sha256Hex } from "./crypto.ts";
import type { Session } from "./session.ts";
import { signRs256, verifyRs256, type Jwk } from "./ship/jwt.ts";
import type { Repo } from "./repo.ts";
import type { OidcCode, Org } from "./types.ts";

export interface OidcConfig {
  signingKey: Record<string, unknown>;
  clients: Record<string, { secret: string; redirectUri: string }>;
}

export const CODE_TTL_SECONDS = 120;
export const ID_TOKEN_TTL_SECONDS = 600;
export const SUPPORTED_SCOPES = ["openid", "email", "profile", "groups"] as const;

function requireString(v: unknown, field: string): string {
  if (typeof v !== "string" || !v) throw new Error(`signing key is missing ${field}`);
  return v;
}

/** The private JWK's own `n`/`e`/`kid` are the public key; strip `d`/`p`/`q`/... for /oidc/jwks. */
export function publicJwk(signingKey: Record<string, unknown>): Jwk {
  return {
    kty: requireString(signingKey.kty, "kty"),
    kid: requireString(signingKey.kid, "kid"),
    n: requireString(signingKey.n, "n"),
    e: requireString(signingKey.e, "e"),
    alg: "RS256",
    use: "sig",
  };
}

async function importSigningKey(signingKey: Record<string, unknown>): Promise<CryptoKey> {
  return crypto.subtle.importKey("jwk", signingKey, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["sign"]);
}

export function parseScope(raw: string | null): string {
  const requested = (raw ?? "openid").split(/\s+/).filter(Boolean);
  const kept = requested.filter((s) => (SUPPORTED_SCOPES as readonly string[]).includes(s));
  return (kept.includes("openid") ? kept : ["openid", ...kept]).join(" ");
}

/** Gitea's group-claim mapping (`--group-claim-name groups --admin-group railyard-admins`): every signed-in user gets `railyard-users`; org owners also get `railyard-admins`. */
export function groupsFor(org: Pick<Org, "owner_user_id">, session: Pick<Session, "uid">): string[] {
  return session.uid === org.owner_user_id ? ["railyard-users", "railyard-admins"] : ["railyard-users"];
}

/** Creates and stores a single-use authorization code; returns the raw code for the redirect URL. */
export async function issueCode(
  repo: Repo,
  input: { clientId: string; redirectUri: string; org: Org; session: Session; scope: string; nonce: string | null },
  now: Date,
): Promise<string> {
  const raw = randomToken(32);
  const code: Omit<OidcCode, "created_at" | "consumed_at"> = {
    code_hash: await sha256Hex(raw),
    client_id: input.clientId,
    redirect_uri: input.redirectUri,
    org_id: input.org.id,
    user_id: input.session.uid,
    email: input.session.email,
    nonce: input.nonce,
    scope: input.scope,
    expires_at: new Date(now.getTime() + CODE_TTL_SECONDS * 1000).toISOString(),
  };
  await repo.createOidcCode(code);
  return raw;
}

export interface Claims {
  sub: string;
  email: string;
  email_verified: true;
  groups: string[];
  org: string;
  scope: string;
}

async function claimsFor(repo: Repo, code: OidcCode): Promise<Claims> {
  const org = await repo.getOrg(code.org_id);
  const groups = org ? groupsFor(org, { uid: code.user_id }) : ["railyard-users"];
  return { sub: code.user_id, email: code.email, email_verified: true, groups, org: code.org_id, scope: code.scope };
}

/** Redeems a code once (backlog item 5): verifies the client secret, consumes the code, mints id_token + access_token. */
export async function exchangeCode(
  repo: Repo,
  cfg: OidcConfig,
  input: { code: string; clientId: string; clientSecret: string; redirectUri: string; issuer: string },
  now: Date,
): Promise<{ id_token: string; access_token: string; token_type: "Bearer"; expires_in: number; scope: string } | { error: string }> {
  const client = cfg.clients[input.clientId];
  if (!client || client.secret !== input.clientSecret) return { error: "invalid_client" };
  const stored = await repo.consumeOidcCode(await sha256Hex(input.code), now.toISOString());
  if (!stored) return { error: "invalid_grant" };
  if (stored.client_id !== input.clientId || stored.redirect_uri !== input.redirectUri) return { error: "invalid_grant" };
  const claims = await claimsFor(repo, stored);
  const key = await importSigningKey(cfg.signingKey);
  const kid = requireString(cfg.signingKey.kid, "kid");
  const iat = Math.floor(now.getTime() / 1000);
  const exp = iat + ID_TOKEN_TTL_SECONDS;
  const idToken = await signRs256(key, { iss: input.issuer, sub: claims.sub, aud: input.clientId, exp, iat, ...(stored.nonce ? { nonce: stored.nonce } : {}), email: claims.email, email_verified: true, groups: claims.groups }, kid);
  const accessToken = await signRs256(key, { iss: input.issuer, sub: claims.sub, aud: input.clientId, exp, iat, typ: "at+jwt", scope: claims.scope, groups: claims.groups, email: claims.email, email_verified: true }, kid);
  return { id_token: idToken, access_token: accessToken, token_type: "Bearer", expires_in: ID_TOKEN_TTL_SECONDS, scope: claims.scope };
}

/** Verifies a bearer access_token minted by `exchangeCode` and returns userinfo claims. */
export async function userinfo(cfg: OidcConfig, token: string, issuer: string): Promise<Record<string, unknown> | null> {
  let aud: unknown;
  try {
    aud = JSON.parse(atob(token.split(".")[1]?.replace(/-/g, "+").replace(/_/g, "/") ?? "")).aud;
  } catch {
    return null;
  }
  if (typeof aud !== "string" || !cfg.clients[aud]) return null;
  const check = await verifyRs256(token, [publicJwk(cfg.signingKey)], { issuer, audience: aud, now: new Date() });
  if (!check.ok) return null;
  const { sub, email, email_verified, groups } = check.claims;
  return { sub, email, email_verified, groups, name: email };
}
