// Project-scoped server keys for server-side forwarders (QY1.2, Wave Q
// contract 5.3). A Scribase forwarder (Purchases, QU1.4) sends its events to
// `POST /v1/server/:id/batch` with `Authorization: Bearer ry_pak_<hex>`,
// never with the public site key. The key is HMAC-SHA256 of the project id
// under the console secret both the console and this service hold, so it
// needs no table; the console hands it only to the Scribase control plane
// over the signed bridge, which keeps it as an environment secret. Changing
// PRODUCT_ANALYTICS_CONSOLE_SECRET rotates every server key at once.

import { createHmac, timingSafeEqual } from "node:crypto";

export const SERVER_KEY_PREFIX = "ry_pak_";
const SERVER_KEY = /^ry_pak_[0-9a-f]{64}$/;

export function serverKeyFor(secret: string, projectId: string): string {
  return `${SERVER_KEY_PREFIX}${createHmac("sha256", secret).update(`pa-server-key:v1:${projectId}`).digest("hex")}`;
}

/** Constant-time check of a presented key for one project. */
export function verifyServerKey(secret: string | null, projectId: string, presented: string | null): boolean {
  if (!secret || !presented || !SERVER_KEY.test(presented)) return false;
  const want = Buffer.from(serverKeyFor(secret, projectId));
  const got = Buffer.from(presented);
  return want.length === got.length && timingSafeEqual(want, got);
}
