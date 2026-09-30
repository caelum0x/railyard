// Destination secrets at rest: AES-256-GCM under PA_EXPORTS_KEY (32 bytes,
// base64 or hex). Sealed form: "v1.<iv>.<tag>.<ciphertext>" in base64url.
// The key never leaves the service and a secret is shown only when created.

import { createCipheriv, createDecipheriv, randomBytes } from "node:crypto";

export class SecretsUnavailable extends Error {}

export function exportsKey(env: Record<string, string | undefined> = process.env): Buffer {
  const raw = env.PA_EXPORTS_KEY?.trim();
  if (!raw) throw new SecretsUnavailable("PA_EXPORTS_KEY is not set");
  const key = /^[0-9a-fA-F]{64}$/.test(raw) ? Buffer.from(raw, "hex") : Buffer.from(raw, "base64");
  if (key.length !== 32) throw new SecretsUnavailable("PA_EXPORTS_KEY must be 32 bytes (64 hex characters or base64)");
  return key;
}

export function seal(plain: string, key: Buffer = exportsKey()): string {
  const iv = randomBytes(12);
  const c = createCipheriv("aes-256-gcm", key, iv);
  const ct = Buffer.concat([c.update(plain, "utf8"), c.final()]);
  return ["v1", iv.toString("base64url"), c.getAuthTag().toString("base64url"), ct.toString("base64url")].join(".");
}

export function open(sealed: string, key: Buffer = exportsKey()): string {
  const [v, iv, tag, ct] = sealed.split(".");
  if (v !== "v1" || !iv || !tag || ct === undefined) throw new Error("sealed secret has an unknown format");
  const d = createDecipheriv("aes-256-gcm", key, Buffer.from(iv, "base64url"));
  d.setAuthTag(Buffer.from(tag, "base64url"));
  return Buffer.concat([d.update(Buffer.from(ct, "base64url")), d.final()]).toString("utf8");
}

/** A Standard Webhooks signing secret: whsec_ + 24 random bytes in base64. */
export const newSigningSecret = (): string => `whsec_${randomBytes(24).toString("base64")}`;
