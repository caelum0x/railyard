import { deps } from "../../../lib/deps.ts";
import { cookie } from "../../../lib/http.ts";
import {
  EMAIL_COOKIE,
  EMAIL_COOKIE_TTL_SECONDS,
  normalizeEmail,
  PKCE_COOKIE,
  PROVIDERS,
  sendEmailCode,
  startSignIn,
  type Provider,
} from "../../../lib/scribase-auth.ts";
import { normalizeSrc, SRC_COOKIE, SRC_COOKIE_TTL_SECONDS } from "../../../lib/session.ts";

export const dynamic = "force-dynamic";

/**
 * One entry point for every "come sign up" deep link, including the
 * cross-sell bridge from other Railyard products (console cross-sell
 * bridge, RYP-X1): `/auth/start?provider=google`, or
 * `/auth/start?email=jane@acme.com&src=insights` to prefill and immediately
 * send an email sign-in code, carrying `src` through to the new org's
 * `signup_source` (see lib/session.ts).
 */
export async function GET(req: Request) {
  const { cfg } = deps();
  const url = new URL(req.url);
  const provider = url.searchParams.get("provider") as Provider;
  const email = normalizeEmail(url.searchParams.get("email"));
  const src = normalizeSrc(url.searchParams.get("src"));
  const srcCookie = src ? [cookie(SRC_COOKIE, src, SRC_COOKIE_TTL_SECONDS)] : [];

  if (PROVIDERS.includes(provider)) {
    const { url: authorizeUrl, verifier } = await startSignIn({ scribaseUrl: cfg.scribaseUrl, consoleUrl: cfg.consoleUrl, provider });
    const headers = new Headers({ location: authorizeUrl });
    headers.append("set-cookie", cookie(PKCE_COOKIE, verifier, 600));
    for (const c of srcCookie) headers.append("set-cookie", c);
    return new Response(null, { status: 302, headers });
  }

  if (email) {
    try {
      await sendEmailCode({ scribaseUrl: cfg.scribaseUrl, anonKey: cfg.scribaseAnonKey, email });
    } catch (err) {
      console.error(`cross-sell sign-in: ${(err as Error).message}`);
      return new Response(null, { status: 302, headers: { location: `${cfg.consoleUrl}/?signin=failed` } });
    }
    const headers = new Headers({ location: `${cfg.consoleUrl}/?step=code` });
    headers.append("set-cookie", cookie(EMAIL_COOKIE, encodeURIComponent(email), EMAIL_COOKIE_TTL_SECONDS));
    for (const c of srcCookie) headers.append("set-cookie", c);
    return new Response(null, { status: 302, headers });
  }

  // Neither a recognized provider nor an email was supplied -- this is the
  // common case for the Insights cross-sell banner (RYP-X1) and the public
  // comparison/Rithub landing pages, which link here with only `?src=<slug>`
  // when the visitor has no known email yet. Previously this fell through to
  // a raw 400 with a developer-facing message, which meant every one of
  // those marketing CTAs 404/400'd end-to-end for a logged-out visitor
  // instead of landing them on the real sign-in page -- found auditing those
  // pages' "Start free" CTA against actual behavior. Preserve the `src`
  // attribution cookie and send the visitor to the normal sign-in page,
  // where they can pick a provider or type their email; `auth/callback` and
  // `lib/email-signin.ts` already read this same cookie at completion time.
  const headers = new Headers({ location: `${cfg.consoleUrl}/` });
  for (const c of srcCookie) headers.append("set-cookie", c);
  return new Response(null, { status: 302, headers });
}
