// Email sign-in (no Google or GitHub app needed): the form posts an address,
// Scribase Auth mails a one-time code, the second form posts the code. The
// handlers take their dependencies so the tests run them without Next.

import { cookie, readCookie, sameOrigin } from "./http.ts";
import type { Repo } from "./repo.ts";
import {
  EMAIL_COOKIE,
  EMAIL_COOKIE_TTL_SECONDS,
  normalizeCode,
  normalizeEmail,
  sendEmailCode,
  verifyEmailCode,
} from "./scribase-auth.ts";
import { normalizeSrc, SESSION_COOKIE, SESSION_TTL_SECONDS, SRC_COOKIE } from "./session.ts";
import { completeSignIn } from "./signin.ts";

export interface EmailSignInDeps {
  cfg: { consoleUrl: string; scribaseUrl: string; scribaseAnonKey: string; sessionSecret: string };
  repo: Repo;
  fetchImpl?: typeof fetch;
  now?: () => Date;
}

function redirect(location: string, cookies: string[] = []): Response {
  const headers = new Headers({ location });
  for (const c of cookies) headers.append("set-cookie", c);
  return new Response(null, { status: 303, headers });
}

async function formField(req: Request, name: string): Promise<unknown> {
  try {
    return (await req.formData()).get(name);
  } catch {
    return null;
  }
}

/** POST /auth/email: send a code to the typed address, then show the code step. */
export async function startEmailSignIn(req: Request, d: EmailSignInDeps): Promise<Response> {
  const base = d.cfg.consoleUrl;
  if (!sameOrigin(req, base)) return new Response("Forbidden", { status: 403 });
  const email = normalizeEmail(await formField(req, "email"));
  if (!email) return redirect(`${base}/?signin=email`);
  try {
    await sendEmailCode({ scribaseUrl: d.cfg.scribaseUrl, anonKey: d.cfg.scribaseAnonKey, email, fetchImpl: d.fetchImpl });
  } catch (err) {
    console.error(`email sign-in: ${(err as Error).message}`);
    return redirect(`${base}/?signin=failed`);
  }
  return redirect(`${base}/?step=code`, [cookie(EMAIL_COOKIE, encodeURIComponent(email), EMAIL_COOKIE_TTL_SECONDS)]);
}

/** POST /auth/email/verify: redeem the code and open the session. */
export async function verifyEmailSignIn(req: Request, d: EmailSignInDeps): Promise<Response> {
  const base = d.cfg.consoleUrl;
  if (!sameOrigin(req, base)) return new Response("Forbidden", { status: 403 });
  const raw = readCookie(req, EMAIL_COOKIE);
  let email: string | null = null;
  try {
    email = raw ? normalizeEmail(decodeURIComponent(raw)) : null;
  } catch {
    email = null;
  }
  if (!email) return redirect(`${base}/?signin=expired`, [cookie(EMAIL_COOKIE, "", 0)]);
  const code = normalizeCode(await formField(req, "code"));
  if (!code) return redirect(`${base}/?step=code&signin=code`);
  const src = normalizeSrc(readCookie(req, SRC_COOKIE));
  try {
    const user = await verifyEmailCode({ scribaseUrl: d.cfg.scribaseUrl, anonKey: d.cfg.scribaseAnonKey, email, code, fetchImpl: d.fetchImpl });
    const { cookie: session } = await completeSignIn(d.repo, d.cfg.sessionSecret, user, (d.now ?? (() => new Date()))(), src);
    return redirect(`${base}/apps`, [
      cookie(SESSION_COOKIE, session, SESSION_TTL_SECONDS),
      cookie(EMAIL_COOKIE, "", 0),
      cookie(SRC_COOKIE, "", 0),
    ]);
  } catch (err) {
    console.error(`email sign-in: ${(err as Error).message}`);
    return redirect(`${base}/?step=code&signin=code`);
  }
}
