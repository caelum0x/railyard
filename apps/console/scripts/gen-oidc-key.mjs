#!/usr/bin/env node
// One-time key generation for Console's OIDC provider (backlog item 5).
// Run once, then set the printed value as the RAILYARD_OIDC_SIGNING_KEY
// secret (never commit it). No dependency: Node's built-in WebCrypto.
//
//   node apps/console/scripts/gen-oidc-key.mjs > /tmp/oidc-key.json
//   # paste the single-line JSON into RAILYARD_OIDC_SIGNING_KEY
//
// The same WebCrypto JWK import (crypto.subtle.importKey("jwk", ...)) the
// console's own OIDC routes use at request time (lib/oidc.ts) reads this
// back; the public n/e/kid are re-derived from it for /oidc/jwks, so only
// this one private value needs to be stored.

import { webcrypto as crypto } from "node:crypto";

const pair = await crypto.subtle.generateKey(
  { name: "RSASSA-PKCS1-v1_5", modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: "SHA-256" },
  true,
  ["sign", "verify"],
);
const jwk = await crypto.subtle.exportKey("jwk", pair.privateKey);
jwk.kid = `railyard-oidc-${Date.now().toString(36)}`;
jwk.alg = "RS256";
jwk.use = "sig";
process.stdout.write(`${JSON.stringify(jwk)}\n`);
