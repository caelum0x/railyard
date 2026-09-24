# Fallback routing

`tensorzero.toml` in this folder shows how Relay keeps a request alive when a
provider fails:

1. **Retries.** A failed provider call is retried on the same provider with
   exponential backoff and jitter. A provider's `Retry-After` header sets the
   wait, capped at `max_delay_s`. Configure with
   `retries = { num_retries, max_delay_s }` on the model. The default is one
   retry. Set `num_retries = 0` to fail over immediately.
2. **Provider failover.** A model lists providers in `routing`. When the first
   is still failing after its retries, or its timeout expires, the same request
   goes to the next provider.
3. **Fallback variant.** A variant listed in the function's
   `[experimentation] fallback_variants` is never sampled. It runs only after
   the sampled variant fails, so an OpenAI outage can fall back to Claude.
4. **Hedging (optional).** With `hedge.enabled = true`, Relay starts a second
   provider after `stagger_ms` and keeps whichever answers first. Losing
   attempts are cancelled. `max_cost_micros` bounds what concurrent attempts
   may cost.

Once any model enables hedging, Relay also tracks every provider's latency and
error rate. A provider that keeps failing has its circuit opened and is ranked
last for a cooldown period, then probed again.
`GET /v1/relay/provider_health` shows the current state.

## Run it

You need `OPENAI_API_KEY` and `ANTHROPIC_API_KEY`. `OPENROUTER_API_KEY` is
optional and adds the OpenRouter leg to each model.

```bash
# From the repository root
export OPENAI_API_KEY=sk-... ANTHROPIC_API_KEY=sk-ant-...
docker compose -f docker-compose.quickstart.yml build
docker compose -f docker-compose.quickstart.yml run --rm --service-ports \
  -v "$PWD/examples/relay/fallback-routing:/app/example:ro" \
  -e ANTHROPIC_API_KEY \
  --entrypoint gateway gateway --config-file /app/example/tensorzero.toml
```

## See failover happen

Break the primary provider on purpose by starting the gateway with an invalid
OpenAI key and a valid Anthropic key. Leave `OPENROUTER_API_KEY` unset for this
demo, otherwise the OpenRouter leg answers before the fallback variant runs.

```bash
OPENAI_API_KEY=sk-invalid docker compose -f docker-compose.quickstart.yml run --rm \
  --service-ports -v "$PWD/examples/relay/fallback-routing:/app/example:ro" \
  -e ANTHROPIC_API_KEY --entrypoint gateway gateway \
  --config-file /app/example/tensorzero.toml
```

Then call the function. OpenAI rejects the key, and the `anthropic` fallback
variant answers:

```bash
curl -sS http://localhost:3000/inference -H "Content-Type: application/json" \
  -d '{"function_name":"chat","input":{"messages":[{"role":"user","content":"Which model are you?"}]}}'
```

The response's `variant_name` is `anthropic`. The gateway log shows the failed
OpenAI attempts (one try plus one retry) before the fallback variant ran.
