import { deps } from "../../../lib/deps.ts";
import { cookie } from "../../../lib/http.ts";
import { PKCE_COOKIE, PROVIDERS, startSignIn, type Provider } from "../../../lib/scribase-auth.ts";

export const dynamic = "force-dynamic";

export async function GET(req: Request) {
  const { cfg } = deps();
  const provider = new URL(req.url).searchParams.get("provider") as Provider;
  if (!PROVIDERS.includes(provider)) return new Response("Unknown sign-in provider", { status: 400 });
  const { url, verifier } = await startSignIn({ scribaseUrl: cfg.scribaseUrl, consoleUrl: cfg.consoleUrl, provider });
  return new Response(null, { status: 302, headers: { location: url, "set-cookie": cookie(PKCE_COOKIE, verifier, 600) } });
}
