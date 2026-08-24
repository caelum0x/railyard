# Relay Ruby SDK

Native Ruby client for the **Relay** inference gateway (a TensorZero fork). It
speaks the gateway's OpenAI-compatible HTTP surface directly — **no dependency
on the `openai` gem**.

```ruby
require "relay"

client = Relay::Client.new(
  base_url: "http://localhost:3000",     # gateway root
  api_key:  ENV["RELAY_API_KEY"]         # sent as: Authorization: Bearer <key>
)
```

Every request hits the gateway's OpenAI-compatible routes under
`#{base_url}/openai/v1/...` with `Content-Type: application/json` and, when an
`api_key` is set, `Authorization: Bearer <api_key>`.

## Chat completions (JSON)

```ruby
resp = client.chat_completions(
  model:    Relay.model_target("gpt-4o-mini"),  # => "tensorzero::model_name::gpt-4o-mini"
  messages: [{ role: "user", content: "Write a haiku about Ruby." }],
  temperature: 0.7,
  max_tokens: 128
)

resp["choices"][0]["message"]["content"]
resp["usage"]  # { "prompt_tokens" => .., "completion_tokens" => .., "total_tokens" => .. }
```

`:model` and `:messages` are required. Relay-specific keys such as
`"tensorzero::episode_id"` pass through to the gateway untouched.

## Chat completions (SSE streaming)

Sets `stream: true` and requests usage in the final chunk
(`stream_options.include_usage: true`). Each decoded chunk Hash is yielded; the
SSE `[DONE]` sentinel terminates the stream.

```ruby
client.chat_completions_stream(
  model:    Relay.model_target("gpt-4o-mini"),
  messages: [{ role: "user", content: "Stream me a limerick." }]
) do |chunk|
  delta = chunk.dig("choices", 0, "delta", "content")
  print delta if delta
  # the final chunk carries "usage" when include_usage is on
end
```

Without a block it returns an `Enumerator`:

```ruby
stream = client.chat_completions_stream(model: "m", messages: msgs)
first_chunk = stream.first
```

## Models

> **Gateway gap.** The Relay gateway exposes **no public list-models route**
> (there is no `GET /openai/v1/models`). Rather than fabricate an OpenAI
> `/v1/models` payload, `#models` returns the static list you configure at
> construction.

```ruby
client = Relay::Client.new(base_url: url, known_models: ["gpt-4o-mini", "claude"])
client.models # => ["gpt-4o-mini", "claude"]

Relay.model_target("gpt-4o-mini")    # "tensorzero::model_name::gpt-4o-mini"
Relay.function_target("summarize")   # "tensorzero::function_name::summarize"
```

## Credits / balance

> **Gateway gap.** There is **no dedicated balance route**. Billing is an
> optional gateway middleware whose only client-visible signal is a
> `402 insufficient_credits` on inference. Rather than invent a balance number,
> `#credits` / `#balance` probe that signal with a minimal chat completion.

```ruby
client.credits(probe_model: Relay.model_target("gpt-4o-mini"))
# => { sufficient: true,  checked_via: "inference_probe" }
# => { sufficient: false, checked_via: "inference_probe", type: "insufficient_credits", ... }
```

If `known_models` is configured, its first entry is used as the default probe
model. A probe costs one (tiny, `max_tokens: 1`) inference.

## Errors

| HTTP | Raised                              |
|------|-------------------------------------|
| 401 / 403 | `Relay::AuthenticationError`   |
| 402 (or `error.type == "insufficient_credits"`) | `Relay::InsufficientCreditsError` (< `Relay::BillingError`) |
| 404 | `Relay::NotFoundError`              |
| 429 | `Relay::RateLimitError`            |
| 5xx | `Relay::ServerError`               |
| other non-2xx | `Relay::APIError`         |

All inherit from `Relay::APIError` (itself a `Relay::Error`) and carry
`#status`, `#type`, `#code`, and `#body`.

## Custom transport

The client accepts any object responding to `#request` and `#stream`
(see `Relay::NetHTTPTransport`). This is how the test suite runs with zero
network access:

```ruby
Relay::Client.new(base_url: url, transport: MyTransport.new)
```

## Development

```bash
bundle install      # or just use the bundled minitest
rake test
```

Tests use an injected in-memory transport (`test/support/fake_transport.rb`) —
no live gateway or `webmock` required.

## License

Apache-2.0
