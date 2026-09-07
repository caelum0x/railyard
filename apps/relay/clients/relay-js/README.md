# @relay/sdk

Native JavaScript/TypeScript client for Railyard Relay. It uses the platform
`fetch` implementation and has no runtime dependencies.

## Install

```bash
pnpm add @relay/sdk
```

Node 18+, modern browsers, React Native, and Expo runtimes with `fetch`,
`ReadableStream`, `TextEncoder`, and `TextDecoder` are supported.

## Paid inference

```ts
import { RelayClient } from "@relay/sdk";

const relay = new RelayClient({
  baseUrl: process.env.RELAY_URL!,
  apiKey: process.env.RELAY_API_KEY,
  settlementEvidenceVerifier: (evidence) =>
    mintVerifier.verifyAttestation(evidence, pinnedMintPublicKey),
});

const completion = await relay.paidChatCompletions(
  {
    model: "tensorzero::model_name::openai::gpt-4o",
    messages: [{ role: "user", content: "Summarize this." }],
  },
  {
    // Private keys stay in the wallet/signer implementation. The SDK only
    // receives the encoded authorization returned for this exact challenge.
    authorize: (payment) => wallet.authorizeAitp(payment),
  },
);
```

The paid methods perform exactly one `402 → authorize → retry` journey. A
second challenge is returned as `PaymentRequiredError`; the SDK never loops or
silently spends again. Paid helpers cover chat, chat streaming, native
inference, embeddings, batch creation, and binary multipart extension calls.

```ts
const form = new MultipartForm()
  .text("purpose", "batch")
  .file("file", "requests.jsonl", "application/jsonl", bytes);
await relay.paidMultipart("v1/files", form, authorizer);
```

Multipart bodies are encoded once and replayed byte-for-byte after a payment
challenge, including the original boundary.

A `409 request_already_settled` is promoted to `AlreadySettledError` only after
`settlementEvidenceVerifier` approves Mint's signed attestation. Without a
verifier it is `UnverifiedSettlementEvidenceError`; a rejected signature is
`InvalidSettlementEvidenceError`. The verifier must use caller-pinned Mint key
material, never a key learned from the evidence itself.

## Streaming and recovery

```ts
const abandoned: string[] = [];
const relay = new RelayClient({
  baseUrl: "https://relay.example",
  apiKey: "…",
  onAbandonedStream: (requestId) => abandoned.push(requestId),
});

for await (const chunk of relay.paidChatCompletionsStream(params, authorizer)) {
  // consume chunks
}
```

Relay withholds `[DONE]` until settlement intent is durable. If a consumer
drops a stream before `[DONE]`, the callback receives the authenticated request
ID synchronously. Put it on an application-owned queue and later call
`settlementEvidence(requestId)`, which applies the configured verifier; do not
perform asynchronous I/O in the callback itself. Recovery systems that already
own a verifier pipeline may call the explicitly unsafe
`settlementEvidenceUnverified(requestId)` and must validate before marking the
request settled.

## Discovery and operations

```ts
const status = await relay.status();
const health = await relay.health(); // typed body on both HTTP 200 and 503
const catalog = await relay.catalog();
const quote = await relay.quote({
  model: "openai/gpt-4o",
  input_tokens: 1_000,
  output_tokens: 500,
});

const deadLetters = await relay.costEvents({
  state: "dead_lettered",
  limit: 50,
});
await relay.replayCostEvent(deadLetters.items[0].event.event_id);
```

Cost-event inspection, listing, and replay are principal-scoped by the Relay
API key. Listing uses a bounded `limit` (1–100) and the returned
`next_cursor` as the next page's `before` value.

## Other request families

- `chatCompletions` / `chatCompletionsStream`
- `embeddings`
- `inference`
- `batchInference` / `batchInferenceStatus`
- `settlementEvidence`
- `costEvent` / `costEvents` / `replayCostEvent`
- `status` / `health` / `catalog` / `quote`
- `multipart` / `paidMultipart` for safe relative upload endpoints

`models()` remains as a backward-compatible synchronous view of
`knownModels`; new code should call `catalog()` for Relay's live,
billing-consistent snapshot. `credits()` is also compatibility-only: it uses a
dry-run availability probe and never invents a numeric balance.

## Safety boundaries

- Base URLs must be absolute HTTP(S) URLs without embedded credentials, query,
  or fragment.
- Redirects are rejected so authorization headers cannot cross origins.
- JSON and SSE event sizes are bounded.
- API keys and AITP authorizations are never exposed through a debug formatter.
- AITP request IDs and operator pagination cursors must be UUIDs.

Run `pnpm typecheck`, `pnpm test`, and `pnpm build` for the full local check.
