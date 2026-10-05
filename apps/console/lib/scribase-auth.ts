// Sign-in through Scribase Auth (the console's own Scribase project): Google or
// GitHub (the OAuth authorize redirect with PKCE, then the code exchange), or a
// one-time code sent by email (POST /otp, then POST /verify). Every path ends
// with a server-side /user read. Server-only.

import { randomToken, sha256B64url } from "./crypto.ts";

export type Provider = "google" | "github";
export const PROVIDERS: Provider[] = ["google", "github"];
export const PKCE_COOKIE = "ry_pkce";
/** Holds the address a code was sent to, between the two email sign-in steps. */
export const EMAIL_COOKIE = "ry_email";
export const EMAIL_COOKIE_TTL_SECONDS = 900;

/**
 * The OAuth buttons to show: RAILYARD_OAUTH_PROVIDERS (comma separated, default
 * "google,github"). Empty hides both, for a project whose OAuth apps are not set
 * up yet; email sign-in is always offered.
 */
export function oauthProvidersFrom(raw: string | undefined): Provider[] {
  if (raw === undefined) return [...PROVIDERS];
  const wanted = raw.split(",").map((s) => s.trim().toLowerCase());
  return PROVIDERS.filter((p) => wanted.includes(p));
}

/** A plain, bounded email address in lower case, or null. */
export function normalizeEmail(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  const email = raw.trim().toLowerCase();
  if (email.length < 3 || email.length > 254) return null;
  return /^[^\s@<>()",;:]+@[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+$/.test(email) ? email : null;
}

/** A one-time code as typed: letters and digits only, 4 to 12 of them, or null. */
export function normalizeCode(raw: unknown): string | null {
  if (typeof raw !== "string") return null;
  const code = raw.replace(/\s+/g, "");
  return /^[A-Za-z0-9]{4,12}$/.test(code) ? code : null;
}

type Fetch = typeof fetch;

export async function startSignIn(opts: { scribaseUrl: string; consoleUrl: string; provider: Provider }): Promise<{ url: string; verifier: string }> {
  if (!PROVIDERS.includes(opts.provider)) throw new Error("unknown provider");
  const verifier = randomToken(48);
  const challenge = await sha256B64url(verifier);
  const q = new URLSearchParams({
    provider: opts.provider,
    redirect_to: `${opts.consoleUrl}/auth/callback`,
    code_challenge: challenge,
    code_challenge_method: "s256",
  });
  return { url: `${opts.scribaseUrl}/auth/v1/authorize?${q}`, verifier };
}

export interface SignedInUser {
  id: string;
  email: string;
}

export async function finishSignIn(opts: {
  scribaseUrl: string;
  anonKey: string;
  code: string;
  verifier: string;
  fetchImpl?: Fetch;
}): Promise<SignedInUser> {
  const f = opts.fetchImpl ?? fetch;
  const tokenRes = await f(`${opts.scribaseUrl}/auth/v1/token?grant_type=pkce`, {
    method: "POST",
    headers: { apikey: opts.anonKey, "content-type": "application/json" },
    body: JSON.stringify({ auth_code: opts.code, code_verifier: opts.verifier }),
  });
  if (!tokenRes.ok) throw new Error(`sign-in code exchange failed (${tokenRes.status})`);
  const token = (await tokenRes.json()) as { access_token?: string };
  if (!token.access_token) throw new Error("sign-in returned no session");
  return readUser(f, opts.scribaseUrl, opts.anonKey, token.access_token);
}

/** Step 1 of email sign-in: Scribase Auth mails a one-time code (enumeration-safe, rate limited there). */
export async function sendEmailCode(opts: { scribaseUrl: string; anonKey: string; email: string; fetchImpl?: Fetch }): Promise<void> {
  const f = opts.fetchImpl ?? fetch;
  const res = await f(`${opts.scribaseUrl}/auth/v1/otp`, {
    method: "POST",
    headers: { apikey: opts.anonKey, "content-type": "application/json" },
    body: JSON.stringify({ email: opts.email, create_user: true }),
  });
  if (!res.ok) throw new Error(`could not send the sign-in code (${res.status})`);
}

/** Step 2 of email sign-in: redeem the code for a session, then read the user. */
export async function verifyEmailCode(opts: {
  scribaseUrl: string;
  anonKey: string;
  email: string;
  code: string;
  fetchImpl?: Fetch;
}): Promise<SignedInUser> {
  const f = opts.fetchImpl ?? fetch;
  const res = await f(`${opts.scribaseUrl}/auth/v1/verify`, {
    method: "POST",
    headers: { apikey: opts.anonKey, "content-type": "application/json" },
    body: JSON.stringify({ type: "email", email: opts.email, token: opts.code }),
  });
  if (!res.ok) throw new Error(`the sign-in code was not accepted (${res.status})`);
  const token = (await res.json()) as { access_token?: string };
  if (!token.access_token) throw new Error("sign-in returned no session");
  const user = await readUser(f, opts.scribaseUrl, opts.anonKey, token.access_token);
  if (user.email !== opts.email) throw new Error("the signed-in user does not match the address the code was sent to");
  return user;
}

async function readUser(f: Fetch, scribaseUrl: string, anonKey: string, accessToken: string): Promise<SignedInUser> {
  // The user record comes from the auth server, never from the token body.
  const userRes = await f(`${scribaseUrl}/auth/v1/user`, {
    headers: { apikey: anonKey, authorization: `Bearer ${accessToken}` },
  });
  if (!userRes.ok) throw new Error(`could not read the signed-in user (${userRes.status})`);
  const user = (await userRes.json()) as { id?: string; email?: string };
  if (!user.id || !user.email) throw new Error("the signed-in user has no id or email");
  return { id: user.id, email: user.email.toLowerCase() };
}

export function orgNameFor(email: string): string {
  const local = email.split("@")[0] ?? "my";
  return `${local.slice(0, 60)}'s apps`;
}
