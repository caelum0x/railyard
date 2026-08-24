# Relay Gateway Clients

Native client SDKs for the **Relay** inference gateway (a TensorZero fork). Each
SDK speaks the gateway's OpenAI-compatible HTTP surface directly — no dependency
on the `openai` package/gem — targeting `POST /openai/v1/chat/completions` for
both JSON and SSE streaming responses.

Models and functions are addressed via the `model` field using Relay's
`tensorzero::model_name::<m>` / `tensorzero::function_name::<f>` scheme.

## SDKs

| Directory     | Language              | Package        | Status |
|---------------|-----------------------|----------------|--------|
| `relay-js`    | JavaScript/TypeScript | `@relay/sdk`   | Real client + tests |
| `relay-ruby`  | Ruby                  | `relay`        | Real client + tests |
| `openai-node` | JavaScript/TypeScript | (OpenAI SDK)   | Interop example |
| `openai-go`   | Go                    | (OpenAI SDK)   | Interop example |

Both `relay-js` and `relay-ruby` implement:

- **chat** — non-streaming chat completions (`chatCompletions` / `chat_completions`)
- **stream** — SSE streaming chat completions with `stream_options.include_usage`
  (`chatCompletionsStream` / `chat_completions_stream`)
- **models** — returns the configured `knownModels` catalog annotated with Relay
  targets (the gateway exposes no public list-models route)
- **credits/balance** — probes the billing middleware's `402 insufficient_credits`
  signal with a minimal inference (the gateway exposes no dedicated balance route)

Both map non-2xx responses to a typed error hierarchy (insufficient-credits, auth,
not-found, rate-limit, server, generic API errors) and pass through
`tensorzero::*` extension params untouched.

## relay-js

```bash
cd relay-js
pnpm install
pnpm test      # vitest
pnpm build     # tsc -> dist/
```

```ts
import { RelayClient } from "@relay/sdk";

const client = new RelayClient({
  baseUrl: "http://localhost:3000",
  apiKey: process.env.RELAY_API_KEY,
});

const res = await client.chatCompletions({
  model: "tensorzero::model_name::gpt-4o-mini",
  messages: [{ role: "user", content: "Hi" }],
});

for await (const chunk of client.chatCompletionsStream({
  model: "tensorzero::model_name::gpt-4o-mini",
  messages: [{ role: "user", content: "Hi" }],
})) {
  process.stdout.write(chunk.choices[0]?.delta.content ?? "");
}
```

## relay-ruby

```bash
cd relay-ruby
bundle install
rake test      # minitest
```

```ruby
require "relay"

client = Relay::Client.new(
  base_url: "http://localhost:3000",
  api_key: ENV["RELAY_API_KEY"],
)

res = client.chat_completions(
  model: Relay.model_target("gpt-4o-mini"),
  messages: [{ role: "user", content: "hi" }],
)

client.chat_completions_stream(
  model: Relay.model_target("gpt-4o-mini"),
  messages: [{ role: "user", content: "hi" }],
) { |chunk| print chunk.dig("choices", 0, "delta", "content") }
```

## OpenAI-compatible clients

Because the gateway exposes an OpenAI-compatible surface, you can also drive it
with standard OpenAI clients by pointing their base URL at the gateway. See
`openai-node` and `openai-go` for interop examples, or use the HTTP API from any
language.
