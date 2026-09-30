// Standard Webhooks verification, the same scheme as the console bridge
// (apps/console/lib/standard-webhooks.ts): HMAC-SHA256 over
// `${webhook-id}.${webhook-timestamp}.${body}` with the base64 key after
// `whsec_`; `v1,<base64>` signatures; five minutes of tolerance.

import { createHmac, timingSafeEqual } from "node:crypto";

export const TOLERANCE_SECONDS = 300;

export interface WebhookHeaders {
  id: string | null;
  timestamp: string | null;
  signature: string | null;
}

const key = (secret: string) => Buffer.from(secret.trim().replace(/^whsec_/, ""), "base64");

export function sign(secret: string, id: string, timestamp: string, body: string): string {
  return `v1,${createHmac("sha256", key(secret)).update(`${id}.${timestamp}.${body}`).digest("base64")}`;
}

export function verify(secret: string, h: WebhookHeaders, body: string, nowSeconds: number): boolean {
  if (!h.id || !h.timestamp || !h.signature) return false;
  const sent = Number(h.timestamp.trim());
  if (!Number.isInteger(sent) || Math.abs(nowSeconds - sent) > TOLERANCE_SECONDS) return false;
  const k = key(secret);
  if (k.length === 0) return false;
  const expected = Buffer.from(createHmac("sha256", k).update(`${h.id}.${h.timestamp.trim()}.${body}`).digest("base64"));
  return h.signature
    .split(/\s+/)
    .filter((c) => c.startsWith("v1,"))
    .some((c) => {
      const got = Buffer.from(c.slice(3));
      return got.length === expected.length && timingSafeEqual(got, expected);
    });
}
