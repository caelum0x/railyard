// The signed-in user for server components and route handlers.

import { cookies } from "next/headers";
import { deps } from "./deps.ts";
import { openSession, SESSION_COOKIE, type Session } from "./session.ts";
import { readCookie } from "./http.ts";

export async function currentSession(): Promise<Session | null> {
  return openSession(deps().cfg.sessionSecret, cookies().get(SESSION_COOKIE)?.value, new Date());
}

export async function sessionFromRequest(req: Request): Promise<Session | null> {
  return openSession(deps().cfg.sessionSecret, readCookie(req, SESSION_COOKIE), new Date());
}
