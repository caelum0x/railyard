// Shared by both sign-in completion paths (OAuth callback and email code
// verify): where to land right after a session is opened. Normally `/apps`;
// if the visit started at `/oidc/authorize` and had to detour through
// sign-in (backlog item 5), land back there instead so the authorization
// code is issued immediately, and clear the detour cookie either way.

import { cookie } from "./http.ts";
import { OIDC_RETURN_COOKIE } from "./session.ts";

export function postSignInLocation(consoleUrl: string, oidcReturnCookieValue: string | undefined | null): { location: string; clearCookie: string } {
  const clearCookie = cookie(OIDC_RETURN_COOKIE, "", 0);
  if (!oidcReturnCookieValue) return { location: `${consoleUrl}/apps`, clearCookie };
  try {
    const search = decodeURIComponent(oidcReturnCookieValue);
    if (!search.startsWith("client_id=")) return { location: `${consoleUrl}/apps`, clearCookie };
    return { location: `${consoleUrl}/oidc/authorize?${search}`, clearCookie };
  } catch {
    return { location: `${consoleUrl}/apps`, clearCookie };
  }
}
