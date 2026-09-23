/**
 * Ed25519 signing over WebCrypto (`globalThis.crypto.subtle`), available in
 * Node >= 20, Deno, Bun and modern browsers — no packages required.
 */

// PKCS#8 DER prefix for an Ed25519 private key wrapping a 32-byte seed.
const PKCS8_PREFIX = Uint8Array.of(
  0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
);

function subtle(): SubtleCrypto {
  const s = globalThis.crypto?.subtle;
  if (!s) throw new Error("WebCrypto (crypto.subtle) is not available in this runtime");
  return s;
}

export function b64urlEncode(bytes: Uint8Array): string {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function b64urlDecode(text: string): Uint8Array {
  const t = text.trim().replace(/-/g, "+").replace(/_/g, "/");
  const bin = atob(t + "=".repeat((4 - (t.length % 4)) % 4));
  return Uint8Array.from(bin, (c) => c.charCodeAt(0));
}

export function hexEncode(bytes: Uint8Array): string {
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

export function hexDecode(hex: string): Uint8Array {
  const clean = hex.trim();
  if (clean.length % 2 !== 0 || /[^0-9a-fA-F]/.test(clean)) throw new Error("invalid hex string");
  const out = new Uint8Array(clean.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(clean.slice(i * 2, i * 2 + 2), 16);
  return out;
}

/** An imported Ed25519 key pair built from a 32-byte secret seed. */
export interface Ed25519Key {
  privateKey: CryptoKey;
  publicKey: Uint8Array;
}

export async function importSeed(seed: Uint8Array): Promise<Ed25519Key> {
  if (seed.length !== 32) throw new Error("an Ed25519 secret seed must be exactly 32 bytes");
  const der = new Uint8Array(PKCS8_PREFIX.length + 32);
  der.set(PKCS8_PREFIX);
  der.set(seed, PKCS8_PREFIX.length);
  const privateKey = await subtle().importKey("pkcs8", der, { name: "Ed25519" }, true, ["sign"]);
  const jwk = await subtle().exportKey("jwk", privateKey);
  if (!jwk.x) throw new Error("could not derive the Ed25519 public key");
  return { privateKey, publicKey: b64urlDecode(jwk.x) };
}

export async function signEd25519(key: CryptoKey, message: Uint8Array<ArrayBuffer>): Promise<Uint8Array<ArrayBuffer>> {
  return new Uint8Array(await subtle().sign({ name: "Ed25519" }, key, message));
}
