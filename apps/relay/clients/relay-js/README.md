# @relay/sdk

Native JavaScript/TypeScript client for the **Relay** inference gateway. It
wraps the gateway's OpenAI-compatible HTTP surface directly (no `openai`
dependency) using the global `fetch`.

## Install

```bash
pnpm add @relay/sdk   # or npm install @relay/sdk
```

## Usage

```ts
import { RelayClient } from "@relay/sdk";

const client = new RelayClient({
  baseUrl: "http://localhost:3000", // gateway root
  apiKey: process.env.RELAY_API_KEY,
  knownModels: ["gpt-4o-mini"],
});

// Non-streaming chat completion.
const res = await client.chatCompletions({
  model: "tensorzero::model_name::gpt-4o-mini",
  messages: [{ role: "user", content: "Hello" }],
});
console.log(res.choices[0].message.content, res.usage);

// Streaming chat completion (async iterator over SSE deltas).
for await (const chunk of client.chatCompletionsStream({
  model: "tensorzero::model_name::gpt-4o-mini",
  messages: [{ role: "user", content: "Stream please" }],
})) {
  process.stdout.write(chunk.choices[0]?.delta.content ?? "");
  if (chunk.usage) console.log("\nusage:", chunk.usage);
}
```

## API

### `new RelayClient({ baseUrl, apiKey?, fetch?, knownModels?, timeoutMs? })`

- `baseUrl` — gateway root; chat calls hit `${baseUrl}/openai/v1/chat/completions`.
- `apiKey` — sent as `Authorization: Bearer <apiKey>` when set.
- `fetch` — inject a custom fetch (used by the test suite; defaults to global `fetch`).
- `knownModels` — catalog returned by `models()` (see caveat below).

### `chatCompletions(params) => Promise<ChatCompletion>`

POST with `stream:false`; returns the parsed OpenAI-compatible response.
`model` and a non-empty `messages` array are required. Relay-native params are
passed through under the `tensorzero::*` namespace (e.g. `tensorzero::episode_id`).

### `chatCompletionsStream(params) => AsyncGenerator<ChatCompletionChunk>`

POST with `stream:true` and `stream_options.include_usage:true`. Yields typed
delta chunks parsed from the `text/event-stream` body and stops at `data: [DONE]`.
The terminal chunk carries `usage`.

### `models() => RelayModel[]`

Returns the `knownModels` catalog annotated with each target's Relay address
(`tensorzero::model_name::*`).

> **Gateway gap:** Relay exposes **no** public list-models route. `models()`
> therefore reflects the catalog you supply rather than fabricating an OpenAI
> `/v1/models` payload from the network.

### `credits(probeModel) => Promise<CreditStatus>` / `balance(probeModel)`

Reads credit *availability*.

> **Gateway gap:** Relay exposes **no** `/credits` or `/balance` route. Billing
> is enforced by middleware that returns `402 { error: { type: "insufficient_credits" } }`
> on inference. `credits()` issues a minimal probe inference (`max_tokens:1`) and
> normalizes the result: `hasCredits:false` on a 402, otherwise `hasCredits:true`.
> `balance` is always `null` — no numeric balance is invented.

## Errors

Non-2xx responses throw a typed error:

- `402` / `insufficient_credits` → `InsufficientCreditsError`
- `401` / `403` → `AuthError`
- anything else → `RelayApiError`

All carry `.status`, `.type`, `.code`, and the raw `.body`.

## Testing

Tests use a mocked `fetch` (no live gateway):

```bash
pnpm install
pnpm test
```
