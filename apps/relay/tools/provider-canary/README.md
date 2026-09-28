# Relay provider-contract canary (RY-RL1)

Relay runs on a frozen fork of TensorZero (upstream archived 2026-06-11), so a
provider API change is ours to catch. This canary is the early warning named in
[`docs/adr/RY-0001-relay-engine.md`](../../../../docs/adr/RY-0001-relay-engine.md).

## What one run does

1. Reads which providers have a key: `GET /v1/relay/provider_keys` (any Relay key).
   A provider listed in `RELAY_CANARY_REQUIRED` (default `anthropic,groq,openrouter`)
   without a key fails the `providers` stage.
2. For every named chat route in `config/tensorzero.toml` (`[models.*]`), calls the
   gateway the way Rally and Menivor do (`tensorzero::model_name::<route>` on
   `/openai/v1/chat/completions`) three times:
   - `plain`: non-streaming, 5 output tokens;
   - `stream`: SSE, must end with `[DONE]`, 5 output tokens;
   - `tool`: a forced call of a no-argument `ping` tool, 48 output tokens (a tool call
     does not fit in 5 tokens on every provider).
3. For every fallback leg (second and later providers, deduplicated), one plain call
   through the provider shorthand (`openrouter::anthropic/claude-sonnet-4.5`).

Every call sends `tensorzero::include_raw_usage`, `tensorzero::cache_options.enabled = off`
and the tag `relay_canary = provider-contract`. A call passes only when the provider
that served it (`tensorzero_raw_usage[].provider_type`) is the route's first configured
provider. So when the Anthropic key is revoked, `claude-sonnet` still answers through
OpenRouter for customers, and the canary turns red with
`claude-sonnet plain: served by openrouter, expected anthropic`.

Every Anthropic-first route (`claude-sonnet`, `claude-haiku`, `claude-sonnet-5`) turns red
together when the Anthropic key breaks; the detail names the first and counts the rest.

A route whose primary has no key and is not required (today: `openai` for `gpt-4o` and
`gpt-4o-mini`) is checked on its next configured provider instead. A route with no
configured provider at all is skipped and counted in the detail.

Today's config gives 9 routes x 3 calls + 7 fallback legs = 34 calls a day.

## Output

Canary format v1 (`infra-box/monitoring/canary-format.md`), canary id
`railyard.relay-providers`, product `railyard`. The infra-box runner
(`monitoring/canaries/relay-providers.sh`, timer `canary@relay-providers.timer`, daily)
pages the ntfy `NTFY_URL_RAILYARD` topic on red and pushes per-route latency to
VictoriaMetrics as `canary_stage_seconds{canary="railyard.relay-providers",stage="<route>"}`.

## Route list

`routes.json` is generated from `config/tensorzero.toml` and is used when the Python on
the box has no `tomllib` (Python 3.10 or older). After any route change:

```sh
python3 apps/relay/tools/provider-canary/provider_canary.py --write-routes
```

The unit tests fail while `routes.json` is stale.

## Tests

```sh
python3 -m unittest discover -s apps/relay/tools/provider-canary/tests
bash ../infra-box/monitoring/canaries/test/relay-providers.test.sh   # runner + e2e
```

`tests/fake_relay.py` is a local stand-in for the gateway (provider list, routing with
failover, streaming, tool calls). No network and no keys.

## Owner setup (once)

1. Create a dedicated Relay key for the canary (UI API Keys page or
   `gateway --create-api-key`) and put a small cap on it with
   `infra-box/stacks/railyard/relay-tenancy.sh`.
2. Store only the key in `/etc/products/relay-canary.key` on box 2 (root, 0600).
3. Add an Uptime Kuma push monitor `railyard.relay-providers` (heartbeat 1500 min) and
   set `CANARY_PUSH_RELAY_PROVIDERS` in `/etc/products/canary.env`.
4. `monitoring/canaries/install.sh relay-providers`.
