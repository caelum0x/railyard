// Signed session cookie: base64url(JSON).base64url(HMAC-SHA256). Server-only.

import { b64url, constantTimeEqual, hmacSha256 } from "./crypto.ts";

export const SESSION_COOKIE = "ry_session";
export const SESSION_TTL_SECONDS = 7 * 24 * 3600;

/**
 * First-touch signup attribution (console cross-sell bridge, RYP-X1): a short
 * lived cookie set when a sign-in link carries `?src=<slug>` (for example the
 * Insights dashboard banner linking to `/auth/start?email=&src=insights`).
 * Read once at the moment a *new* org is created, written to its
 * `signup_source` column, then discarded; it never changes which org an
 * existing user signs into.
 */
export const SRC_COOKIE = "ry_src";
export const SRC_COOKIE_TTL_SECONDS = 1800;

/** A bounded, lower-case attribution slug (e.g. "insights"), or null. */
export function normalizeSrc(raw: string | null | undefined): string | null {
  if (typeof raw !== "string") return null;
  const src = raw.trim().toLowerCase();
  return /^[a-z0-9_-]{1,40}$/.test(src) ? src : null;
}

export interface Session {
  uid: string;
  email: string;
  org: string;
  exp: number;
}

const enc = new TextEncoder();
const dec = new TextDecoder();

function fromB64url(s: string): string {
  const pad = s.replace(/-/g, "+").replace(/_/g, "/");
  return dec.decode(Uint8Array.from(atob(pad + "===".slice((pad.length + 3) % 4)), (c) => c.charCodeAt(0)));
}

export async function sealSession(secret: string, s: Omit<Session, "exp">, now: Date): Promise<string> {
  const body = b64url(enc.encode(JSON.stringify({ ...s, exp: Math.floor(now.getTime() / 1000) + SESSION_TTL_SECONDS })));
  return `${body}.${b64url(await hmacSha256(`${secret}:session`, body))}`;
}

export async function openSession(secret: string, cookie: string | undefined | null, now: Date): Promise<Session | null> {
  if (!cookie) return null;
  const [body, mac] = cookie.split(".");
  if (!body || !mac) return null;
  if (!constantTimeEqual(mac, b64url(await hmacSha256(`${secret}:session`, body)))) return null;
  try {
    const s = JSON.parse(fromB64url(body)) as Session;
    if (typeof s.uid !== "string" || typeof s.org !== "string" || s.exp < now.getTime() / 1000) return null;
    return s;
  } catch {
    return null;
  }
}
