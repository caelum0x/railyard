// Standard Webhooks verification (the scheme Dodo Payments signs with).
// Signed content: `${webhook-id}.${webhook-timestamp}.${body}`, HMAC-SHA256
// with the base64 key after the `whsec_` prefix. `webhook-signature` holds
// space-separated `v1,<base64>` values; any match is accepted. Timestamps
// more than five minutes away are refused. Replays of the same id are
// refused by the caller (billing_events.webhook_id is unique).

import { b64decode, b64encode, constantTimeEqual, hmacSha256 } from "./crypto.ts";

export const TOLERANCE_SECONDS = 300;

export interface WebhookHeaders {
  id: string | null;
  timestamp: string | null;
  signature: string | null;
}

function key(secret: string): Uint8Array | null {
  const raw = secret.trim().replace(/^whsec_/, "");
  const k = b64decode(raw);
  return k && k.length > 0 ? k : null;
}

export async function sign(secret: string, id: string, timestamp: string, body: string): Promise<string> {
  const k = key(secret);
  if (!k) throw new Error("webhook secret is not valid base64");
  return `v1,${b64encode(await hmacSha256(k, `${id}.${timestamp}.${body}`))}`;
}

export async function verify(secret: string, h: WebhookHeaders, body: string, nowSeconds: number): Promise<boolean> {
  if (!h.id || !h.timestamp || !h.signature) return false;
  const sent = Number(h.timestamp.trim());
  if (!Number.isInteger(sent) || Math.abs(nowSeconds - sent) > TOLERANCE_SECONDS) return false;
  const k = key(secret);
  if (!k) return false;
  const expected = b64encode(await hmacSha256(k, `${h.id}.${h.timestamp.trim()}.${body}`));
  return h.signature
    .split(/\s+/)
    .filter((c) => c.startsWith("v1,"))
    .some((c) => constantTimeEqual(c.slice(3), expected));
}
