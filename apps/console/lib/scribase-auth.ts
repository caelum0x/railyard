// Sign-in with Google or GitHub through Scribase Auth (the console's own
// Scribase project): the OAuth authorize redirect with PKCE, then the code
// exchange and a server-side /user read. Server-only.

import { randomToken, sha256B64url } from "./crypto.ts";

export type Provider = "google" | "github";
export const PROVIDERS: Provider[] = ["google", "github"];
export const PKCE_COOKIE = "ry_pkce";

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
  // The user record comes from the auth server, never from the token body.
  const userRes = await f(`${opts.scribaseUrl}/auth/v1/user`, {
    headers: { apikey: opts.anonKey, authorization: `Bearer ${token.access_token}` },
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
