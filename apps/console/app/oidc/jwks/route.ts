// GET /oidc/jwks (backlog item 5): the public key Gitea's auto-discovery
// fetches to verify id_token/access_token signatures.

import { deps } from "../../../lib/deps.ts";
import { json } from "../../../lib/http.ts";
import { publicJwk } from "../../../lib/oidc.ts";

export async function GET() {
  const { cfg } = deps();
  if (!cfg.oidc) return json({ error: "invalid_request" }, 503);
  return json({ keys: [publicJwk(cfg.oidc.signingKey)] }, 200, { "cache-control": "public, max-age=3600" });
}
