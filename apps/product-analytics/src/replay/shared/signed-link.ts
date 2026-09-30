// Signed, expiring, no-login links for one resource of one project (the
// shared-link shape of LIMRUN-GAPS G2): HMAC-SHA256 over
// "<resource>\n<project>\n<exp>" with a key derived from the console secret,
// exp at most 10 minutes ahead. Query: r, p, exp, sig (base64url).

import { createHmac, timingSafeEqual } from "node:crypto";
import { isUuid } from "../../db.ts";

export const LINK_TTL_SECONDS = 600;
export const RESOURCE = /^(replay|heatmap):[A-Za-z0-9._~%-]{1,2048}$/;

const keyOf = (secret: string) => createHmac("sha256", secret).update("pa-share-link-v1").digest();

function mac(secret: string, resource: string, project: string, exp: number): string {
  return createHmac("sha256", keyOf(secret)).update(`${resource}\n${project}\n${exp}`).digest("base64url");
}

export interface SignedLink {
  query: string;
  expires_at: string;
}

export function signLink(secret: string, resource: string, project: string, now: Date, ttlSeconds = LINK_TTL_SECONDS): SignedLink {
  if (!RESOURCE.test(resource)) throw new Error("invalid link resource");
  if (!isUuid(project)) throw new Error("invalid project");
  const exp = Math.floor(now.getTime() / 1000) + Math.min(ttlSeconds, LINK_TTL_SECONDS);
  const q = new URLSearchParams({ r: resource, p: project, exp: String(exp), sig: mac(secret, resource, project, exp) });
  return { query: q.toString(), expires_at: new Date(exp * 1000).toISOString() };
}

/** The link's resource and project when the signature holds and it has not expired; else null. */
export function verifyLink(secret: string, q: URLSearchParams, now: Date): { resource: string; project: string; exp: number } | null {
  const resource = q.get("r") ?? "";
  const project = q.get("p") ?? "";
  const expText = q.get("exp") ?? "";
  const sig = q.get("sig") ?? "";
  if (!RESOURCE.test(resource) || !isUuid(project) || !/^\d{1,12}$/.test(expText) || !/^[A-Za-z0-9_-]{43}$/.test(sig)) return null;
  const exp = Number(expText);
  const nowS = Math.floor(now.getTime() / 1000);
  if (exp <= nowS || exp - nowS > LINK_TTL_SECONDS) return null;
  const want = Buffer.from(mac(secret, resource, project, exp));
  const got = Buffer.from(sig);
  if (want.length !== got.length || !timingSafeEqual(want, got)) return null;
  return { resource, project, exp };
}
