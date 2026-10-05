// Outbound requests to customer destinations go only to public addresses:
// https, no credentials in the URL, and a host that resolves to no private,
// loopback, link-local, metadata or otherwise reserved address.
//
// The host is resolved ONCE per request (resolvePinned) and the connection
// is then made to that exact validated address (guarded-fetch.ts pins it in
// the socket's lookup), so a DNS answer that changes between the check and
// the connect (DNS rebinding) cannot reach an internal address. Redirects
// are never followed. PA_EXPORTS_ALLOW_PRIVATE=1 lifts the address rule for
// local development and tests only (the connection is still pinned).
//
// URL forms such as http://2130706433/, http://0177.0.0.1/, http://0x7f.1/
// and http://[::ffff:127.0.0.1]/ are normalised by the WHATWG URL parser to
// their canonical address before any check, and checked as addresses.

import { lookup } from "node:dns/promises";
import { isIP } from "node:net";

export class DestinationRefused extends Error {}

type V4 = [number, number, number, number];

// [network, prefix bits]
const V4_BLOCKED: [V4, number][] = [
  [[0, 0, 0, 0], 8], // "this" network
  [[10, 0, 0, 0], 8], // private
  [[100, 64, 0, 0], 10], // carrier-grade NAT (includes 100.100.100.200 metadata)
  [[127, 0, 0, 0], 8], // loopback
  [[169, 254, 0, 0], 16], // link-local (includes 169.254.169.254 metadata)
  [[172, 16, 0, 0], 12], // private
  [[192, 0, 0, 0], 24], // IETF protocol assignments
  [[192, 0, 2, 0], 24], // documentation
  [[192, 88, 99, 0], 24], // 6to4 relay anycast
  [[192, 168, 0, 0], 16], // private
  [[198, 18, 0, 0], 15], // benchmarking
  [[198, 51, 100, 0], 24], // documentation
  [[203, 0, 113, 0], 24], // documentation
  [[224, 0, 0, 0], 4], // multicast
  [[240, 0, 0, 0], 4], // reserved + broadcast
];

const toInt = (o: V4): number => ((o[0] << 24) >>> 0) + (o[1] << 16) + (o[2] << 8) + o[3];

function parseV4(ip: string): V4 | null {
  const parts = ip.split(".");
  if (parts.length !== 4) return null;
  const nums = parts.map((p) => (/^\d{1,3}$/.test(p) ? Number(p) : NaN));
  return nums.every((n) => n >= 0 && n <= 255) ? (nums as V4) : null;
}

function v4Blocked(o: V4): boolean {
  const ip = toInt(o);
  return V4_BLOCKED.some(([net, bits]) => {
    const mask = bits === 0 ? 0 : (~0 << (32 - bits)) >>> 0;
    return ((ip & mask) >>> 0) === ((toInt(net) & mask) >>> 0);
  });
}

/** Expands an IPv6 address (with optional embedded dotted IPv4 tail and zone) to 8 hextets. */
export function expandV6(raw: string): number[] | null {
  let ip = raw.toLowerCase().replace(/^\[|\]$/g, "");
  const zone = ip.indexOf("%");
  if (zone >= 0) ip = ip.slice(0, zone);
  const lastColon = ip.lastIndexOf(":");
  if (lastColon < 0) return null;
  if (ip.slice(lastColon + 1).includes(".")) {
    const v4 = parseV4(ip.slice(lastColon + 1));
    if (!v4) return null;
    ip = `${ip.slice(0, lastColon + 1)}${((v4[0] << 8) | v4[1]).toString(16)}:${((v4[2] << 8) | v4[3]).toString(16)}`;
  }
  const halves = ip.split("::");
  if (halves.length > 2) return null;
  const parse = (s: string) => (s === "" ? [] : s.split(":").map((h) => (/^[0-9a-f]{1,4}$/.test(h) ? parseInt(h, 16) : NaN)));
  const head = parse(halves[0]);
  const rest = halves.length === 2 ? parse(halves[1]) : [];
  let groups = head;
  if (halves.length === 2) {
    const fill = 8 - head.length - rest.length;
    if (fill < 1) return null;
    groups = [...head, ...Array<number>(fill).fill(0), ...rest];
  }
  return groups.length === 8 && groups.every((g) => Number.isInteger(g) && g >= 0 && g <= 0xffff) ? groups : null;
}

const embeddedV4 = (g: number[], at: number): V4 => [g[at] >> 8, g[at] & 0xff, g[at + 1] >> 8, g[at + 1] & 0xff];

function v6Blocked(g: number[]): boolean {
  const zeroUpTo = (n: number) => g.slice(0, n).every((x) => x === 0);
  // ::ffff:a.b.c.d (IPv4-mapped) and ::a.b.c.d (IPv4-compatible): judge the IPv4 address.
  if (zeroUpTo(5) && g[5] === 0xffff) return v4Blocked(embeddedV4(g, 6));
  if (zeroUpTo(6)) return true; // ::, ::1 and the deprecated IPv4-compatible block
  // ::ffff:0:a.b.c.d (IPv4-translated, RFC 2765)
  if (zeroUpTo(4) && g[4] === 0xffff && g[5] === 0) return true;
  // 6to4 (2002::/16) carries an IPv4 address: judge it, and refuse it when it is internal.
  if (g[0] === 0x2002) return v4Blocked(embeddedV4(g, 1));
  // Only global unicast 2000::/3 is reachable; this excludes ::/8, 64:ff9b::/96 (NAT64),
  // 100::/64 (discard), fc00::/7 (unique local, includes AWS fd00:ec2::254), fe80::/10,
  // fec0::/10 and ff00::/8 (multicast).
  if ((g[0] & 0xe000) !== 0x2000) return true;
  if (g[0] === 0x2001 && g[1] < 0x0200) return true; // 2001::/23 IETF assignments (Teredo, ORCHID, ...)
  if (g[0] === 0x2001 && g[1] === 0x0db8) return true; // documentation
  if (g[0] === 0x3fff && g[1] < 0x1000) return true; // documentation (3fff::/20)
  return false;
}

/** Whether an address must never be contacted (private, loopback, link-local, metadata, reserved). */
export function isPrivateAddress(ip: string): boolean {
  const kind = isIP(ip.replace(/^\[|\]$/g, "").split("%")[0]);
  if (kind === 4) {
    const v4 = parseV4(ip);
    return v4 ? v4Blocked(v4) : true;
  }
  if (kind === 6) {
    const g = expandV6(ip);
    return g ? v6Blocked(g) : true;
  }
  return true; // not an address at all: refuse
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

export interface Pinned {
  /** The validated address the connection must go to. */
  address: string;
  family: 4 | 6;
  /** The host name as written in the URL (for SNI and the Host header). */
  host: string;
}

export interface GuardOptions {
  /** Resolves a host name to addresses; defaults to the system resolver. Called at most once per request. */
  resolve?: (host: string) => Promise<string[]>;
  /** The address rule; defaults to isPrivateAddress. */
  isBlocked?: (ip: string) => boolean;
  /** Lifts the address rule (still pinned); defaults to PA_EXPORTS_ALLOW_PRIVATE=1. */
  allowPrivate?: () => boolean;
}

const systemResolve = async (host: string): Promise<string[]> => (await lookup(host, { all: true, verbatim: true })).map((a) => a.address);

/** Resolves the URL's host once, refuses any blocked address, and returns the address to connect to. */
export async function resolvePinned(u: URL, opts: GuardOptions = {}): Promise<Pinned> {
  const host = u.hostname.replace(/^\[|\]$/g, "");
  if (!host) throw new DestinationRefused("The address has no host.");
  const addrs = isIP(host) ? [host] : await (opts.resolve ?? systemResolve)(host).catch((err: Error) => {
    throw new DestinationRefused(`${u.hostname} could not be resolved (${err.message}).`.slice(0, 300));
  });
  if (addrs.length === 0) throw new DestinationRefused(`${u.hostname} could not be resolved.`);
  const lifted = (opts.allowPrivate ?? allowPrivate)();
  const blocked = opts.isBlocked ?? isPrivateAddress;
  // Every answer must be public: a mixed answer could be used to pick an internal one.
  if (!lifted && addrs.some((a) => blocked(a))) throw new DestinationRefused(`${u.hostname} is not a public address.`);
  const address = addrs[0];
  const family = isIP(address);
  if (family !== 4 && family !== 6) throw new DestinationRefused(`${u.hostname} resolved to something that is not an address.`);
  return { address, family, host };
}

/** Resolves the host and refuses private addresses (kept for callers that only want the check). */
export async function checkReachable(u: URL, opts: GuardOptions = {}): Promise<void> {
  await resolvePinned(u, opts);
}
