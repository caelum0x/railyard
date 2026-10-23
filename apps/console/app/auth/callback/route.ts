import { deps } from "../../../lib/deps.ts";
import { cookie, readCookie } from "../../../lib/http.ts";
import { finishSignIn, PKCE_COOKIE } from "../../../lib/scribase-auth.ts";
import { normalizeSrc, OIDC_RETURN_COOKIE, SESSION_COOKIE, SESSION_TTL_SECONDS, SRC_COOKIE } from "../../../lib/session.ts";
import { completeSignIn } from "../../../lib/signin.ts";
import { postSignInLocation } from "../../../lib/post-signin.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  const { cfg, repo } = deps();
  const code = new URL(req.url).searchParams.get("code");
  const verifier = readCookie(req, PKCE_COOKIE);
  const src = normalizeSrc(readCookie(req, SRC_COOKIE));
  const back = (q: string) => new Response(null, { status: 302, headers: { location: `${cfg.consoleUrl}/?${q}`, "set-cookie": cookie(PKCE_COOKIE, "", 0) } });
  if (!code || !verifier) return back("signin=expired");
  try {
    const user = await finishSignIn({ scribaseUrl: cfg.scribaseUrl, anonKey: cfg.scribaseAnonKey, code, verifier });
    const { cookie: session } = await completeSignIn(repo, cfg.sessionSecret, user, new Date(), src);
    const { location, clearCookie } = postSignInLocation(cfg.consoleUrl, readCookie(req, OIDC_RETURN_COOKIE));
    const headers = new Headers({ location });
    headers.append("set-cookie", cookie(SESSION_COOKIE, session, SESSION_TTL_SECONDS));
    headers.append("set-cookie", cookie(PKCE_COOKIE, "", 0));
    headers.append("set-cookie", cookie(SRC_COOKIE, "", 0));
    headers.append("set-cookie", clearCookie);
    return new Response(null, { status: 302, headers });
  } catch (err) {
    console.error(`sign-in failed: ${(err as Error).message}`);
    return back("signin=failed");
  }
}
