// GET /.well-known/openid-configuration (backlog item 5): OIDC discovery
// document. This is exactly the URL `apps/gitea/docker-compose.yml`'s
// `TODO(railyard-sso)` comment names as `--auto-discover-url`.

import { deps } from "../../../lib/deps.ts";
import { json } from "../../../lib/http.ts";
import { SUPPORTED_SCOPES } from "../../../lib/oidc.ts";

export async function GET() {
  const { cfg } = deps();
  if (!cfg.oidc) return json({ error: "invalid_request", error_description: "OIDC provider is not configured" }, 503);
  const base = cfg.consoleUrl;
  return json(
    {
      issuer: base,
      authorization_endpoint: `${base}/oidc/authorize`,
      token_endpoint: `${base}/oidc/token`,
      userinfo_endpoint: `${base}/oidc/userinfo`,
      jwks_uri: `${base}/oidc/jwks`,
      response_types_supported: ["code"],
      subject_types_supported: ["public"],
      id_token_signing_alg_values_supported: ["RS256"],
      scopes_supported: [...SUPPORTED_SCOPES],
      claims_supported: ["sub", "email", "email_verified", "groups"],
      grant_types_supported: ["authorization_code"],
      token_endpoint_auth_methods_supported: ["client_secret_basic", "client_secret_post"],
    },
    200,
    { "cache-control": "public, max-age=3600" },
  );
}
