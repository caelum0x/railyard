// Console -> service: a 5-minute HS256 token {org_id, app_ids, role} signed
// with PRODUCT_ANALYTICS_CONSOLE_SECRET. Every console API call is scoped to
// the token's org and to the app ids it names.

import { jwtVerify, SignJWT } from "jose";
import { isUuid } from "../db.ts";

export const CONSOLE_TOKEN_MAX_SECONDS = 300;
export const ROLES = ["owner", "admin", "member", "viewer"] as const;

export interface ConsoleClaims {
  org_id: string;
  app_ids: string[];
  role: (typeof ROLES)[number];
  exp: number;
}

const key = (secret: string) => new TextEncoder().encode(secret);

export async function verifyConsoleToken(secret: string, token: string | null, now: Date): Promise<ConsoleClaims | null> {
  if (!token) return null;
  try {
    const { payload } = await jwtVerify(token, key(secret), { algorithms: ["HS256"], currentDate: now, requiredClaims: ["exp", "iat"] });
    const p = payload as Record<string, unknown>;
    if (typeof p.exp !== "number" || typeof p.iat !== "number" || p.exp - p.iat > CONSOLE_TOKEN_MAX_SECONDS) return null;
    if (!isUuid(p.org_id)) return null;
    if (!Array.isArray(p.app_ids) || p.app_ids.length > 1000 || !p.app_ids.every(isUuid)) return null;
    if (typeof p.role !== "string" || !(ROLES as readonly string[]).includes(p.role)) return null;
    return { org_id: p.org_id, app_ids: p.app_ids as string[], role: p.role as ConsoleClaims["role"], exp: p.exp };
  } catch {
    return null;
  }
}

/** For tests and tools; the console signs with its own WebCrypto code (same format). */
export async function signConsoleToken(secret: string, claims: Omit<ConsoleClaims, "exp">, now: Date, ttlSeconds = CONSOLE_TOKEN_MAX_SECONDS): Promise<string> {
  const iat = Math.floor(now.getTime() / 1000);
  return new SignJWT({ org_id: claims.org_id, app_ids: claims.app_ids, role: claims.role })
    .setProtectedHeader({ alg: "HS256", typ: "JWT" })
    .setIssuedAt(iat)
    .setExpirationTime(iat + ttlSeconds)
    .sign(key(secret));
}
