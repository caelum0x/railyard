// GET /oidc/authorize (backlog item 5): the authorization endpoint Gitea's
// `openidConnect` auth source redirects a user to. Requires an active
// Console session; if there is none, detours through sign-in and comes
// straight back here (lib/session.ts OIDC_RETURN_COOKIE) before issuing the
// code, so the admin/engineer never sees a separate Rithub-only login.

import { currentSession } from "../../../lib/current.ts";
import { deps } from "../../../lib/deps.ts";
import { cookie } from "../../../lib/http.ts";
import { issueCode, parseScope } from "../../../lib/oidc.ts";
import { OIDC_RETURN_COOKIE, OIDC_RETURN_TTL_SECONDS } from "../../../lib/session.ts";

function errorRedirect(redirectUri: string, state: string | null, error: string, description?: string): Response {
  const url = new URL(redirectUri);
  url.searchParams.set("error", error);
  if (description) url.searchParams.set("error_description", description);
  if (state) url.searchParams.set("state", state);
  return new Response(null, { status: 302, headers: { location: url.toString() } });
}

export async function GET(req: Request) {
  const { cfg, repo } = deps();
  const url = new URL(req.url);
  const q = url.searchParams;
  const clientId = q.get("client_id") ?? "";
  const redirectUri = q.get("redirect_uri") ?? "";
  const responseType = q.get("response_type") ?? "";
  const state = q.get("state");
  const nonce = q.get("nonce");

  if (!cfg.oidc) return new Response("OIDC provider is not configured.", { status: 503 });
  const client = cfg.oidc.clients[clientId];
  // redirect_uri is checked for exact equality against the registered value
  // *before* ever redirecting anywhere -- an unregistered or mismatched
  // redirect_uri is a 400 on this page, never a redirect (no open redirect).
  if (!client || client.redirectUri !== redirectUri) {
    return new Response("Unknown client_id or redirect_uri does not match the registered value.", { status: 400 });
  }
  if (responseType !== "code") return errorRedirect(redirectUri, state, "unsupported_response_type", "only `code` is supported");
  const scope = parseScope(q.get("scope"));

  const session = await currentSession();
  if (!session) {
    const headers = new Headers({ location: `${cfg.consoleUrl}/` });
    headers.append("set-cookie", cookie(OIDC_RETURN_COOKIE, encodeURIComponent(q.toString()), OIDC_RETURN_TTL_SECONDS));
    return new Response(null, { status: 302, headers });
  }
  const org = await repo.getOrg(session.org);
  if (!org) return errorRedirect(redirectUri, state, "server_error", "no org for this session");

  const code = await issueCode(repo, { clientId, redirectUri, org, session, scope, nonce }, new Date());
  const location = new URL(redirectUri);
  location.searchParams.set("code", code);
  if (state) location.searchParams.set("state", state);
  return new Response(null, { status: 302, headers: { location: location.toString() } });
}
