// GET /oidc/userinfo (backlog item 5): verifies the access_token minted by
// /oidc/token and returns the same claims as the id_token.

import { deps } from "../../../lib/deps.ts";
import { json } from "../../../lib/http.ts";
import { userinfo } from "../../../lib/oidc.ts";

export async function GET(req: Request) {
  const { cfg } = deps();
  if (!cfg.oidc) return json({ error: "invalid_request" }, 503);
  const auth = req.headers.get("authorization");
  if (!auth?.startsWith("Bearer ")) return new Response(null, { status: 401, headers: { "www-authenticate": 'Bearer realm="railyard-oidc"' } });
  const claims = await userinfo(cfg.oidc, auth.slice(7), cfg.consoleUrl);
  if (!claims) return new Response(null, { status: 401, headers: { "www-authenticate": 'Bearer realm="railyard-oidc", error="invalid_token"' } });
  return json(claims);
}
