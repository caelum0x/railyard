// The console's 5-minute org token for the product analytics service:
// HS256 JWT {org_id, app_ids, role, iat, exp} signed with
// PRODUCT_ANALYTICS_CONSOLE_SECRET (the service verifies it with jose).
// WebCrypto only, so it runs on Workers without a new dependency.

import { b64url, hmacSha256 } from "../../../../lib/crypto.ts";

export const TOKEN_TTL_SECONDS = 300;

export type Role = "owner" | "admin" | "member" | "viewer";

const enc = new TextEncoder();
const part = (v: unknown) => b64url(enc.encode(JSON.stringify(v)));

export async function signOrgToken(secret: string, claims: { org_id: string; app_ids: string[]; role: Role }, now: Date): Promise<string> {
  const iat = Math.floor(now.getTime() / 1000);
  const head = `${part({ alg: "HS256", typ: "JWT" })}.${part({ org_id: claims.org_id, app_ids: claims.app_ids, role: claims.role, iat, exp: iat + TOKEN_TTL_SECONDS })}`;
  return `${head}.${b64url(await hmacSha256(enc.encode(secret), head))}`;
}
