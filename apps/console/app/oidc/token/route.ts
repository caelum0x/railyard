// POST /oidc/token (backlog item 5): exchanges a code from /oidc/authorize
// for an id_token + access_token. Client auth accepts either HTTP Basic
// (`client_secret_basic`) or `client_id`/`client_secret` in the form body
// (`client_secret_post`) -- Gitea's OAuth2 client supports both.

import { deps } from "../../../lib/deps.ts";
import { exchangeCode } from "../../../lib/oidc.ts";
import { json } from "../../../lib/http.ts";

function basicAuth(req: Request): { id: string; secret: string } | null {
  const h = req.headers.get("authorization");
  if (!h?.startsWith("Basic ")) return null;
  try {
    const [id, secret] = atob(h.slice(6)).split(":");
    return id && secret !== undefined ? { id, secret } : null;
  } catch {
    return null;
  }
}

export async function POST(req: Request) {
  const { cfg, repo } = deps();
  if (!cfg.oidc) return json({ error: "invalid_request", error_description: "OIDC provider is not configured" }, 503);

  const ct = req.headers.get("content-type") ?? "";
  const params = ct.includes("application/json") ? new URLSearchParams(Object.entries((await req.json().catch(() => ({}))) as Record<string, string>)) : new URLSearchParams(await req.text());

  if (params.get("grant_type") !== "authorization_code") return json({ error: "unsupported_grant_type" }, 400);
  const code = params.get("code");
  const redirectUri = params.get("redirect_uri");
  const basic = basicAuth(req);
  const clientId = basic?.id ?? params.get("client_id");
  const clientSecret = basic?.secret ?? params.get("client_secret");
  if (!code || !redirectUri || !clientId || !clientSecret) return json({ error: "invalid_request" }, 400);

  const result = await exchangeCode(repo, cfg.oidc, { code, clientId, clientSecret, redirectUri, issuer: cfg.consoleUrl }, new Date());
  if ("error" in result) return json(result, result.error === "invalid_client" ? 401 : 400);
  return json(result, 200, { pragma: "no-cache" });
}
