// Outbound requests to customer destinations go only to public addresses:
// https, no credentials in the URL, and a host that resolves to no private,
// loopback, link-local or otherwise reserved address (checked at send time,
// so a later DNS change is caught too). Redirects are not followed.
// PA_EXPORTS_ALLOW_PRIVATE=1 lifts the address rule for local development
// and tests only.

import { lookup } from "node:dns/promises";
import { isIP } from "node:net";

export class DestinationRefused extends Error {}

function v4Private(ip: string): boolean {
  const [a, b] = ip.split(".").map(Number);
  return a === 0 || a === 10 || a === 127 || (a === 100 && b >= 64 && b <= 127) || (a === 169 && b === 254) || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168) || (a === 192 && b === 0) || (a === 198 && (b === 18 || b === 19)) || a >= 224;
}

export function isPrivateAddress(ip: string): boolean {
  if (isIP(ip) === 4) return v4Private(ip);
  const x = ip.toLowerCase();
  if (x === "::" || x === "::1") return true;
  const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/.exec(x);
  if (mapped) return v4Private(mapped[1]);
  return x.startsWith("fc") || x.startsWith("fd") || x.startsWith("fe8") || x.startsWith("fe9") || x.startsWith("fea") || x.startsWith("feb") || x.startsWith("ff") || x.startsWith("64:ff9b");
}

export const allowPrivate = (env: Record<string, string | undefined> = process.env): boolean => env.PA_EXPORTS_ALLOW_PRIVATE === "1";

/** Validates the shape of a destination URL (at create time). */
export function checkUrlShape(raw: string, what: string): URL {
  let u: URL;
  try {
    u = new URL(raw);
  } catch {
    throw new DestinationRefused(`${what} must be a full URL.`);
  }
  if (u.username || u.password) throw new DestinationRefused(`${what} must not contain a user name or password.`);
  if (u.protocol !== "https:" && !(allowPrivate() && u.protocol === "http:")) throw new DestinationRefused(`${what} must start with https://.`);
  if (u.hash) throw new DestinationRefused(`${what} must not contain a #fragment.`);
  return u;
}

/** Resolves the host and refuses private addresses (at send time). */
export async function checkReachable(u: URL): Promise<void> {
  if (allowPrivate()) return;
  const host = u.hostname.replace(/^\[|\]$/g, "");
  const addrs = isIP(host) ? [{ address: host }] : await lookup(host, { all: true, verbatim: true });
  if (addrs.length === 0 || addrs.some((a) => isPrivateAddress(a.address))) throw new DestinationRefused(`${u.hostname} is not a public address.`);
}
