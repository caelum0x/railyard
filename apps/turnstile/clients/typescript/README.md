# @railyard/turnstile-client (TypeScript)

Agent-side AITP-402 payment client for Turnstile-gated HTTP endpoints and MCP
tools. No dependencies: it uses `fetch`, WebCrypto Ed25519 and a bundled
BLAKE3. It runs on Node >= 22.6 (native TypeScript), Deno, Bun and modern
browsers. On older Node versions, compile it with `tsc` first.

```ts
import { Ed25519Signer, SpendPolicy, TurnstileClient } from "@railyard/turnstile-client";

const signer = await Ed25519Signer.fromSeedHex("key_agent", process.env.AGENT_ED25519_SEED_HEX!);
// Register signer.publicKeyHex with Mint under (payer, "key_agent") once.

const client = new TurnstileClient({
  baseUrl: "https://tools.example.com",
  payer: "acct_agent",
  signer,
  policy: new SpendPolicy({ maxPriceMicros: 50_000, totalBudgetMicros: 5_000_000 }),
});

const catalog = await client.tools();                 // GET /v1/tools (free)
const res = await client.callTool("summarize", { text: "..." });
console.log(res.status, res.receipt?.amount_micros);  // signed Mint receipt

if (res.settlement === "capture_pending") {
  const final = await client.waitForSettlement(res);  // GET /v1/settlements/:id
  console.log(final.phase);
}

const mcp = await client.mcpCall("summarize", { text: "..." }); // POST /mcp tools/call
```

How a call is paid:

1. The unpaid request gets `402` + `AITP-Challenge` (HTTP), or JSON-RPC error
   `-32402` with `data.aitp_challenge` (MCP), or an x402 `PaymentRequired` tool
   result carrying it in `_meta["aitp/challenge"]` (x402-enabled servers).
2. `SpendPolicy` checks the price cap, the allowed payees and currencies, the
   expiry and the total budget. If any check fails it throws
   `PaymentRefused` and signs nothing.
3. The client hashes the authorization intent (BLAKE3 over sorted-key
   canonical JSON, byte-identical to Mint) and signs it with Ed25519.
4. It retries with `AITP-Authorization` or the `_aitp_authorization` tool
   argument. Turnstile has Mint verify the signature and open the hold, runs
   the tool, and captures only what was used.

To use a KMS or HSM instead of a seed, implement the `Signer` interface
(`pubkeyId` plus `sign(opHash)`).

Type-check: `tsc --noEmit -p .`
