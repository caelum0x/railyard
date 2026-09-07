import assert from "node:assert/strict";
import test from "node:test";

import { MintClient } from "../dist/index.js";

function transferProof(overrides = {}) {
  return {
    receipt: {
      body: {
        account_id: "acct_sender",
        account_seq: 3,
        journal_seq: 7,
        op: "Transfer",
        op_hash: Array(32).fill(17),
        amount_micros: 2_000_000,
        from: "acct_sender",
        to: "acct_recipient",
        balance_after_available_micros: 8_000_000,
        balance_after_held_micros: 0,
        ts: 1_700_000_000_000,
        prev_hash: "00".repeat(32),
        signer: "mint-key-1",
        ...overrides,
      },
      hash: "22".repeat(32),
      sig: "33".repeat(64),
    },
    attestation: {
      format: "mint-transfer-attestation-v1",
      payload_base64url: "cGF5bG9hZA",
      digest_sha256: "44".repeat(32),
      signer_key_id: "mint-key-1",
      signature_hex: "55".repeat(64),
    },
  };
}

test("typed transfer sends integer micros and a stable idempotency key", async () => {
  const calls = [];
  const fetcher = async (url, init) => {
    calls.push({ url: String(url), init });
    return new Response(JSON.stringify({
      success: true,
      data: {
        first_seq: 7,
        from_balance: { available_micros: 8_000_000 },
        to_balance: { available_micros: 2_000_000 },
        idempotent_replay: false,
        ...transferProof(),
      },
    }), { status: 200, headers: { "content-type": "application/json" } });
  };
  const client = new MintClient({
    baseUrl: "https://mint.example",
    apiKey: "secret",
    fetch: fetcher,
  });

  const receipt = await client.transfer({
    from: "acct_sender",
    to: "acct_recipient",
    amount_micros: 2_000_000,
    idem_key: "person-send-1",
    ref: "menivor:transfer-1",
  });

  assert.equal(receipt.first_seq, 7);
  assert.equal(receipt.idempotent_replay, false);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].url, "https://mint.example/v1/transfers");
  assert.deepEqual(JSON.parse(calls[0].init.body), {
    from: "acct_sender",
    to: "acct_recipient",
    amount_micros: 2_000_000,
    idem_key: "person-send-1",
    ref: "menivor:transfer-1",
  });
});

test("typed transfer rejects unsafe money before transport", async () => {
  let calls = 0;
  const client = new MintClient({
    baseUrl: "https://mint.example",
    apiKey: "secret",
    fetch: async () => {
      calls += 1;
      throw new Error("should not be called");
    },
  });

  for (const amount_micros of [0, -1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
    assert.throws(
      () => client.transfer({
        from: "acct_sender",
        to: "acct_recipient",
        amount_micros,
        idem_key: "person-send-1",
      }),
      /positive safe integer/,
    );
  }
  assert.equal(calls, 0);
});

test("typed transfer rejects a success response without matching signed proof", async () => {
  for (const data of [
    {
      first_seq: 7,
      from_balance: {},
      to_balance: {},
      idempotent_replay: false,
    },
    {
      first_seq: 7,
      from_balance: {},
      to_balance: {},
      idempotent_replay: false,
      ...transferProof({ amount_micros: 1 }),
    },
    {
      first_seq: 7,
      from_balance: {},
      to_balance: {},
      idempotent_replay: false,
      ...transferProof({ to: "acct_attacker" }),
    },
  ]) {
    const client = new MintClient({
      baseUrl: "https://mint.example",
      apiKey: "secret",
      fetch: async () => new Response(JSON.stringify({ success: true, data }), { status: 200 }),
    });
    await assert.rejects(
      client.transfer({
        from: "acct_sender",
        to: "acct_recipient",
        amount_micros: 2_000_000,
        idem_key: "person-send-1",
      }),
      /Mint transfer proof/,
    );
  }
});

test("public-address transfer resolves recipient before moving money", async () => {
  const calls = [];
  const fetcher = async (url, init) => {
    calls.push({ url: String(url), init });
    const data = calls.length === 1
      ? { account_id: "acct_recipient" }
      : {
          first_seq: 7,
          from_balance: {},
          to_balance: {},
          idempotent_replay: false,
          ...transferProof(),
        };
    return new Response(JSON.stringify({ success: true, data }), { status: 200 });
  };
  const client = new MintClient({ baseUrl: "https://mint.example", apiKey: "secret", fetch: fetcher });
  await client.transferToAddress({
    from: "acct_sender",
    address: "ait1recipient",
    amount_micros: 2_000_000,
    idem_key: "address-send-1",
  });
  assert.equal(calls[0].url, "https://mint.example/v1/addresses/ait1recipient");
  assert.equal(JSON.parse(calls[1].init.body).to, "acct_recipient");
});

test("stream, audit, reserve, and revocation routes are exposed", async () => {
  const calls = [];
  const fetcher = async (url, init) => {
    calls.push({ url: String(url), init });
    const data = String(url).endsWith("/receipts") ? [] : {};
    return new Response(JSON.stringify({ success: true, data }), { status: 200 });
  };
  const client = new MintClient({ baseUrl: "https://mint.example", apiKey: "secret", fetch: fetcher });
  await client.openStream({
    from: "acct_sender", to: "acct_recipient", rate_micros: 5,
    cap_micros: 100, unit: "Unit", idem_key: "open-1",
  });
  await client.revokeAllowance("allow_1", "revoke-1");
  await client.accountReceipts("acct_sender");
  await client.reserve();
  assert.deepEqual(calls.map((call) => new URL(call.url).pathname), [
    "/v1/streams", "/v1/allowances/allow_1/revoke",
    "/v1/accounts/acct_sender/receipts", "/v1/reserve",
  ]);
});
