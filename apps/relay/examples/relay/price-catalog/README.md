# Price catalog

Relay keeps one price list, the catalog. The same catalog answers price
questions from your apps and prices every metered or billed request, so what a
customer is quoted is what they are charged.

`catalog.json` in this folder is an example resale catalog. It lists the
quickstart routes at a markup over the OpenAI list price and disables one
model.

## Run it

```bash
# From the repository root
export OPENAI_API_KEY=sk-...
docker compose -f docker-compose.quickstart.yml build
docker compose -f docker-compose.quickstart.yml run --rm --service-ports \
  -v "$PWD/examples/relay/price-catalog:/app/example:ro" \
  -e RELAY_CATALOG_PATH=/app/example/catalog.json \
  gateway

# In another terminal
./examples/relay/price-catalog/quote.sh
```

The first quote is for 1,000 input and 500 output tokens on the `chat`
function at 0.30 USD / 1.20 USD per million tokens:
`1000 * 300000 / 1e6 + 500 * 1200000 / 1e6 = 900` micro-USD (0.0009 USD).
Quotes are exact integers and round up to the next whole micro-USD.

## Endpoints

| Endpoint | Returns |
| --- | --- |
| `GET /v1/relay/catalog` | The whole snapshot: revision, and every model with capabilities and prices. |
| `POST /v1/relay/quotes` | `{"model", "input_tokens", "output_tokens"}` in, the exact `amount_micros` and the catalog revision out. Unknown or disabled models return `404 relay_model_not_found`. |
| `PUT /v1/relay/catalog/prices` | Operator keys only, needs Postgres. Changes one price at runtime without a redeploy. |
| `POST /v1/relay/catalog/prices/reset` | Drops a runtime price change so the file price applies again. |

With `[gateway.auth] enabled = true`, the catalog and quote endpoints need an
API key like every other route.

## Catalog format

```jsonc
{
  "schema_version": 1,            // must be 1
  "revision": "my-catalog-7",     // any non-empty string, returned with every quote
  "generated_at_ms": 1790208000000,
  "models": [
    {
      "id": "tensorzero::function_name::chat", // a function, a named model, or provider/model
      "provider": "tensorzero",                 // "tensorzero" for functions and named models,
                                                // otherwise the part before the "/"
      "display_name": "Chat",
      "aliases": [],                            // extra ids that resolve to this entry
      "capabilities": { "chat": true, "streaming": true }, // at least one must be true
      "pricing": {
        "input_micro_usd_per_million": 300000,  // integer micro-USD per 1M tokens, never negative
        "output_micro_usd_per_million": 1200000,
        "source": "operator_override"           // operator_override | contract | provider_list
      },
      "context_window_tokens": 128000,          // optional
      "max_output_tokens": 16384,               // optional
      "enabled": true                           // disabled entries cannot be quoted or billed
    }
  ]
}
```

Ids are matched the way requests name them:

- `tensorzero::function_name::chat` prices the `chat` function whatever variant
  or model serves it.
- `tensorzero::model_name::gpt-4o-mini` prices the named model `gpt-4o-mini`
  whichever provider serves it, including failover.
- `openai/gpt-4o-mini` prices direct calls such as
  `tensorzero::model_name::openai::gpt-4o-mini`.

An invalid catalog (wrong schema version, duplicate id, negative price, no
capabilities, provider mismatch) stops the gateway at startup instead of
serving wrong prices.

## Keeping list prices current

Rows with `"source": "provider_list"` can be repriced from OpenRouter's public
model list, which needs no key:

```bash
-e RELAY_CATALOG_REFRESH_URL=openrouter \
-e RELAY_CATALOG_REFRESH_INTERVAL_SECS=3600
```

The refresh keeps the last good prices when the feed is unreachable or looks
wrong, and never changes `operator_override` or `contract` rows. Your markup
rows stay exactly as you set them.

Without `RELAY_CATALOG_PATH`, the gateway uses its built-in catalog of provider
list prices. The Docker image sets `RELAY_CATALOG_PATH=/app/config/catalog.json`.
