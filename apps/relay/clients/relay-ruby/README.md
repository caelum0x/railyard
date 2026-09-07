# Relay Ruby SDK

Dependency-free Ruby 2.6+ client for Railyard Relay. It talks directly to
Relay's OpenAI-compatible and native inference surfaces and uses only Ruby's
standard library at runtime.

## Paid inference

```ruby
require "relay"

relay = Relay::Client.new(
  base_url: "https://relay.example",
  api_key: ENV["RELAY_API_KEY"],
  settlement_evidence_verifier: ->(evidence) {
    mint_verifier.verify_attestation(evidence, pinned_mint_public_key)
  }
)

completion = relay.paid_chat_completions(
  {
    model: Relay.model_target("openai::gpt-4o"),
    messages: [{ role: "user", content: "Summarize this." }]
  },
  authorizer: ->(payment) { wallet.authorize_aitp(payment) }
)
```

Paid helpers cover chat, chat streaming, native inference, embeddings, and
batch creation. They perform exactly one `402 -> authorize -> retry` journey.
A second challenge is returned as `Relay::PaymentRequiredError`; the SDK never
loops or silently spends again. The signing implementation retains private
keys—the Relay SDK receives only the authorization for that exact challenge.
An already-settled replay is trusted only after this verifier approves Mint's
signed attestation. Missing verification raises
`Relay::UnverifiedSettlementEvidenceError`; rejection raises
`Relay::InvalidSettlementEvidenceError`.

## Streaming and recovery

```ruby
recovery_queue = []
relay = Relay::Client.new(
  base_url: "https://relay.example",
  on_abandoned_stream: ->(request_id) { recovery_queue << request_id }
)

relay.paid_chat_completions_stream(params, authorizer: authorizer) do |chunk|
  print chunk.dig("choices", 0, "delta", "content")
end
```

Relay withholds `[DONE]` until settlement intent is durable. If the consumer
leaves before `[DONE]`, the callback receives Relay's authenticated request UUID
synchronously. Put it on an application-owned queue and call
`settlement_evidence(request_id)` later; do not do asynchronous network work in
the callback. That method applies the configured verifier; only recovery code
that performs its own validation should call
`settlement_evidence_unverified(request_id)`. The client forces
`stream_options.include_usage = true` so
successful streams carry the usage needed for exact settlement.

## Discovery and operations

```ruby
health = relay.health # decoded on HTTP 200 and degraded HTTP 503
catalog = relay.catalog
quote = relay.quote(model: "openai/gpt-4o", input_tokens: 1_000, output_tokens: 500)

page = relay.cost_events(state: "dead_lettered", limit: 50)
relay.replay_cost_event(page.fetch("items").first.fetch("event").fetch("event_id"))
```

Supported request families:

- `chat_completions` / `chat_completions_stream`
- `embeddings`
- `inference`
- `batch_inference` / `batch_inference_status`
- `status` / `health` / `catalog` / `quote`
- `settlement_evidence`
- `cost_event` / `cost_events` / `replay_cost_event`
- `custom_json` for safe relative Relay extensions
- `multipart` / `paid_multipart` for binary-safe relative upload endpoints

Multipart forms preserve arbitrary bytes and are encoded once, so a paid retry
replays the same boundary and body:

```ruby
form = Relay::MultipartForm.new.file(
  "file", "requests.jsonl", "application/jsonl", payload
)
relay.paid_multipart("v1/files", form, authorizer: authorizer)
```

`models` remains a backward-compatible local view of `known_models`; new code
should use the live, billing-consistent `catalog`. `credits`/`balance` is also
compatibility-only: it performs a `tensorzero::dryrun` availability probe and
never invents a numeric balance.

## Safety boundaries

- Base URLs must be absolute HTTP(S) URLs without credentials, query, or fragment.
- JSON responses and individual SSE events have configurable size bounds.
- AITP challenges, authorizations, request IDs, and cursors are validated.
- Streaming response metadata is observed before body consumption, so an early
  consumer exit still hands the authoritative Relay request ID to recovery.
- The standard transport wraps timeouts and socket failures in
  `Relay::TransportError`.

## Development

```bash
ruby -Ilib -Itest test/client_test.rb
ruby -Ilib -Itest test/product_test.rb
gem build relay.gemspec
```

The tests use an injected in-memory transport and require no network or keys.

## License

Apache-2.0
