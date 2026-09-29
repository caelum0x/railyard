import { deps } from "../../../lib/deps.ts";
import { cookie, sameOrigin } from "../../../lib/http.ts";
import { SESSION_COOKIE } from "../../../lib/session.ts";

export async function POST(req: Request) {
  const { cfg } = deps();
  if (!sameOrigin(req, cfg.consoleUrl)) return new Response("Forbidden", { status: 403 });
  return new Response(null, { status: 303, headers: { location: `${cfg.consoleUrl}/`, "set-cookie": cookie(SESSION_COOKIE, "", 0) } });
}
