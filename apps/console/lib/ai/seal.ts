// Sealing the customer's own provider keys (Wave P PY1.4). AES-256-GCM with
// WebCrypto under RAILYARD_CREDENTIALS_KEY (32 bytes, base64); the org and the
// provider are bound as associated data, so a row copied to another org or
// provider does not open. Only the last four characters are ever read back.

import { b64decode, b64encode } from "../crypto.ts";

const enc = new TextEncoder();
const dec = new TextDecoder();

async function importKey(masterB64: string): Promise<CryptoKey> {
  const raw = b64decode(masterB64.trim());
  if (!raw || raw.length !== 32) throw new Error("RAILYARD_CREDENTIALS_KEY must be 32 bytes, base64");
  return crypto.subtle.importKey("raw", raw, { name: "AES-GCM" }, false, ["encrypt", "decrypt"]);
}

export async function seal(masterB64: string, aad: string, plaintext: string): Promise<{ nonce: string; ciphertext: string }> {
  const key = await importKey(masterB64);
  const iv = crypto.getRandomValues(new Uint8Array(12));
  const ct = new Uint8Array(await crypto.subtle.encrypt({ name: "AES-GCM", iv, additionalData: enc.encode(aad) }, key, enc.encode(plaintext)));
  return { nonce: b64encode(iv), ciphertext: b64encode(ct) };
}

export async function open(masterB64: string, aad: string, nonce: string, ciphertext: string): Promise<string> {
  const key = await importKey(masterB64);
  const iv = b64decode(nonce);
  const ct = b64decode(ciphertext);
  if (!iv || !ct) throw new Error("sealed value is not base64");
  return dec.decode(await crypto.subtle.decrypt({ name: "AES-GCM", iv, additionalData: enc.encode(aad) }, key, ct));
}

export function hint(secret: string): string {
  return `...${secret.slice(-4)}`;
}
