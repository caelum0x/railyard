import assert from "node:assert/strict";
import test from "node:test";

import {
  AuthorizationBuilder,
  PaidHttpClient,
  decodeAuthorization,
} from "../dist/index.js";

test("signs one challenge and retries with the same body", async () => {
  const seen = [];
  const challenge = {
    payee: "ait1qrelay",
    amount_micros: 4200,
    currency: "AIT",
    resource: "POST /v1/chat#model",
    nonce: "nonce-1",
    expiry: Date.now() + 60_000,
    scheme: "aitp-hold",
  };
  const encoded = Buffer.from(JSON.stringify(challenge)).toString("base64url");
  const fetcher = async (_url, init) => {
    seen.push(init);
    return seen.length === 1
      ? new Response(null, { status: 402, headers: { "AITP-Challenge": encoded } })
      : Response.json({ ok: true });
  };
  const signer = {
    async signAuthorization(canonical) {
      const keys = Object.keys(JSON.parse(new TextDecoder().decode(canonical)));
      assert.deepEqual(keys, [...keys].sort());
      return { pubkey_id: "key_test", op_hash: "07".repeat(32), sig: "fake-signature" };
    },
  };
  const client = new PaidHttpClient(new AuthorizationBuilder("acct_payer", signer), fetcher);
  const response = await client.send({
    method: "POST",
    url: "https://relay.test/v1/chat",
    body: '{"model":"x"}',
  });
  assert.equal(response.status, 200);
  assert.equal(seen.length, 2);
  assert.equal(seen[0].body, seen[1].body);
  const authorization = decodeAuthorization(new Headers(seen[1].headers).get("AITP-Authorization"));
  assert.equal(authorization.payer, "acct_payer");
  assert.equal(authorization.scheme, "aitp-hold");
  assert.equal(authorization.idempotency_key, "aitp:nonce-1");
});
