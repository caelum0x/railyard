// RS256 JWTs with WebCrypto (Node and Workers alike), for ship on push (PY3.1):
// signing the GitHub App's own JWT and verifying GitHub Actions OIDC tokens.
// No dependency: the two operations are small and fully tested.

import { b64url } from "../crypto.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

export function fromB64url(s: string): Uint8Array {
  const pad = s.replace(/-/g, "+").replace(/_/g, "/");
  const bin = atob(pad + "===".slice((pad.length + 3) % 4));
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

function pemBody(pem: string): Uint8Array {
  const b64 = pem.replace(/-----(BEGIN|END) [A-Z ]+-----/g, "").replace(/\s+/g, "");
  return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
}

function derLength(n: number): number[] {
  if (n < 0x80) return [n];
  const bytes: number[] = [];
  for (let v = n; v > 0; v >>= 8) bytes.unshift(v & 0xff);
  return [0x80 | bytes.length, ...bytes];
}

function der(tag: number, body: Uint8Array | number[]): Uint8Array {
  const b = body instanceof Uint8Array ? [...body] : body;
  return Uint8Array.from([tag, ...derLength(b.length), ...b]);
}

/**
 * GitHub hands out App keys as PKCS#1 ("BEGIN RSA PRIVATE KEY"); WebCrypto
 * imports PKCS#8 only. PKCS#8 = SEQUENCE { 0, SEQUENCE { rsaEncryption, NULL }, OCTET STRING pkcs1 }.
 */
export function pkcs1ToPkcs8(pkcs1: Uint8Array): Uint8Array {
  const version = [0x02, 0x01, 0x00];
  const algorithm = [...der(0x30, [...der(0x06, [0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01]), 0x05, 0x00])];
  return der(0x30, [...version, ...algorithm, ...der(0x04, pkcs1)]);
}

export async function importRsaPrivateKey(pem: string): Promise<CryptoKey> {
  const text = pem.replace(/\\n/g, "\n");
  const raw = pemBody(text);
  const pkcs8 = /BEGIN RSA PRIVATE KEY/.test(text) ? pkcs1ToPkcs8(raw) : raw;
  return crypto.subtle.importKey("pkcs8", pkcs8, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["sign"]);
}

export async function signRs256(key: CryptoKey, payload: Record<string, unknown>, kid?: string): Promise<string> {
  const header = b64url(enc.encode(JSON.stringify({ alg: "RS256", typ: "JWT", ...(kid ? { kid } : {}) })));
  const body = b64url(enc.encode(JSON.stringify(payload)));
  const sig = new Uint8Array(await crypto.subtle.sign("RSASSA-PKCS1-v1_5", key, enc.encode(`${header}.${body}`)));
  return `${header}.${body}.${b64url(sig)}`;
}

export interface Jwk {
  kty: string;
  kid?: string;
  n?: string;
  e?: string;
  alg?: string;
  use?: string;
}

export type JwtCheck = { ok: true; claims: Record<string, unknown> } | { ok: false; error: string };

/**
 * Verifies an RS256 JWT against a key set: signature, `iss`, `aud`, `exp` and
 * `nbf` (60 s leeway). Any other algorithm, a missing kid match or a malformed
 * token is refused.
 */
export async function verifyRs256(token: string, keys: Jwk[], opts: { issuer: string; audience: string; now: Date }): Promise<JwtCheck> {
  const parts = token.split(".");
  if (parts.length !== 3) return { ok: false, error: "malformed token" };
  let header: { alg?: string; kid?: string };
  let claims: Record<string, unknown>;
  try {
    header = JSON.parse(dec.decode(fromB64url(parts[0] as string)));
    claims = JSON.parse(dec.decode(fromB64url(parts[1] as string)));
  } catch {
    return { ok: false, error: "malformed token" };
  }
  if (header.alg !== "RS256") return { ok: false, error: "unsupported algorithm" };
  const jwk = keys.find((k) => k.kty === "RSA" && (!header.kid || k.kid === header.kid));
  if (!jwk?.n || !jwk.e) return { ok: false, error: "unknown signing key" };
  const key = await crypto.subtle.importKey("jwk", { kty: "RSA", n: jwk.n, e: jwk.e, ext: true }, { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" }, false, ["verify"]);
  const valid = await crypto.subtle.verify("RSASSA-PKCS1-v1_5", key, fromB64url(parts[2] as string), enc.encode(`${parts[0]}.${parts[1]}`));
  if (!valid) return { ok: false, error: "bad signature" };
  const nowS = Math.floor(opts.now.getTime() / 1000);
  if (claims.iss !== opts.issuer) return { ok: false, error: "wrong issuer" };
  const aud = claims.aud;
  if (!(aud === opts.audience || (Array.isArray(aud) && aud.includes(opts.audience)))) return { ok: false, error: "wrong audience" };
  if (typeof claims.exp !== "number" || claims.exp + 60 < nowS) return { ok: false, error: "expired" };
  if (typeof claims.nbf === "number" && claims.nbf - 60 > nowS) return { ok: false, error: "not yet valid" };
  return { ok: true, claims };
}
