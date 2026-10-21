// Signed, expiring, no-login links for one resource of one project (the
// shared-link shape of LIMRUN-GAPS G2): HMAC-SHA256 over
// "<scope>\n<resource>\n<project>\n<exp>" with a key derived from the console
// secret. Query: r, p, exp, sig (base64url).
//
// Two scopes, each with its own cap and its own CSP downstream (routes.ts):
//   "console" (default) -- up to 10 minutes, opened only inside the
//     console's own frame (frame-ancestors PA_CONSOLE_ORIGIN). Used for the
//     "open this recording/heatmap in the console" link.
//   "embed" -- up to 7 days, meant to be pasted into a third party's own
//     page (a support tool, a doc, a status page) as an <iframe>, so its
//     page is framable by any origin. A caller must opt in explicitly
//     (signEmbedLink) -- plain signLink never mints an embed-scoped link, so
//     this wider exposure is never a silent side effect of the existing
//     share-link code path.
//
// A console-scoped signature and an embed-scoped signature for the same
// resource/project/exp never collide (the scope is part of the MAC input),
// so a console link can't be replayed as an embed and vice versa.

import { createHmac, timingSafeEqual } from "node:crypto";
import { isUuid } from "../../db.ts";

export type LinkScope = "console" | "embed";

export const LINK_TTL_SECONDS = 600;
export const EMBED_TTL_SECONDS = 7 * 24 * 3600;
export const RESOURCE = /^(replay|heatmap):[\w.~%-]{1,2048}$/;

const CAP: Record<LinkScope, number> = { console: LINK_TTL_SECONDS, embed: EMBED_TTL_SECONDS };

const keyOf = (secret: string) => createHmac("sha256", secret).update("pa-share-link-v1").digest();

function mac(secret: string, scope: LinkScope, resource: string, project: string, exp: number): string {
  return createHmac("sha256", keyOf(secret)).update(`${scope}\n${resource}\n${project}\n${exp}`).digest("base64url");
}

export interface SignedLink {
  query: string;
  expires_at: string;
}

export function signLink(secret: string, resource: string, project: string, now: Date, ttlSeconds = LINK_TTL_SECONDS, scope: LinkScope = "console"): SignedLink {
  if (!RESOURCE.test(resource)) throw new Error("invalid link resource");
  if (!isUuid(project)) throw new Error("invalid project");
  const cap = CAP[scope];
  const exp = Math.floor(now.getTime() / 1000) + Math.max(1, Math.min(ttlSeconds, cap));
  const q = new URLSearchParams({ r: resource, p: project, exp: String(exp), sig: mac(secret, scope, resource, project, exp) });
  return { query: q.toString(), expires_at: new Date(exp * 1000).toISOString() };
}

/** A console-scoped link (<= 10 minutes), opened only inside the console's own frame. */
export const signConsoleLink = (secret: string, resource: string, project: string, now: Date, ttlSeconds = LINK_TTL_SECONDS): SignedLink => signLink(secret, resource, project, now, ttlSeconds, "console");

/** An embed-scoped link (<= 7 days), meant to be pasted as an <iframe> into any third-party page. */
export const signEmbedLink = (secret: string, resource: string, project: string, now: Date, ttlSeconds = EMBED_TTL_SECONDS): SignedLink => signLink(secret, resource, project, now, ttlSeconds, "embed");

/** The link's resource, project and scope when the signature holds for that scope and it has not expired; else null. */
export function verifyLink(secret: string, q: URLSearchParams, now: Date, scope: LinkScope = "console"): { resource: string; project: string; exp: number; scope: LinkScope } | null {
  const resource = q.get("r") ?? "";
  const project = q.get("p") ?? "";
  const expText = q.get("exp") ?? "";
  const sig = q.get("sig") ?? "";
  const cap = CAP[scope];
  if (!RESOURCE.test(resource) || !isUuid(project) || !/^\d{1,12}$/.test(expText) || !/^[A-Za-z0-9_-]{43}$/.test(sig)) return null;
  const exp = Number(expText);
  const nowS = Math.floor(now.getTime() / 1000);
  if (exp <= nowS || exp - nowS > cap) return null;
  const want = Buffer.from(mac(secret, scope, resource, project, exp));
  const got = Buffer.from(sig);
  if (want.length !== got.length || !timingSafeEqual(want, got)) return null;
  return { resource, project, exp, scope };
}
