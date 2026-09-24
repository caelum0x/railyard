# Budgets: per-key monthly spend caps

Give every app or customer its own API key, record what each key spends, and
stop a key when it reaches its monthly cap. This runs without any payment
provider. Relay prices each successful call from its catalog and stores the
result as a cost event in Postgres ("postpaid metering").

What you get:

- `RELAY_METERING_ENABLED=1` records every successful authenticated inference
  as a priced, durable cost event owned by the calling key.
- `PUT /relay/billing/budgets/{key_id}` sets a monthly cap in micro-USD
  (1,000,000 = 1 USD). Months are calendar months in UTC.
- A key at or over its cap gets HTTP `429` with code
  `relay_spend_cap_exceeded` before any provider is called, so a runaway key
  cannot keep spending.
- `GET /relay/billing/summary` returns a key's own spend. Operator keys can
  pass `?scope=all` to see every key.

## Run it

From the repository root:

```bash
export OPENAI_API_KEY=sk-...
compose() { docker compose -f docker-compose.quickstart.yml -f examples/relay/budgets/docker-compose.budgets.yml "$@"; }

# 1. Postgres, schema migrations and the gateway
compose up -d --build

# 2. Create two API keys. Each command prints one key: sk-t0-<public_id>-<secret>
OPERATOR_KEY=$(compose run --rm -T --no-deps --entrypoint gateway migrations --create-api-key)
APP_KEY=$(compose run --rm -T --no-deps --entrypoint gateway migrations --create-api-key)

# 3. Let the operator key manage caps: list its public id, then restart the gateway
export RELAY_OPERATOR_KEY_IDS=$(cut -d- -f3 <<<"$OPERATOR_KEY")
compose up -d gateway

# 4. Set a tiny cap on the app key, spend through it, read spend, remove the cap
OPERATOR_KEY=$OPERATOR_KEY APP_KEY=$APP_KEY ./examples/relay/budgets/budgets.sh
```

Expected output: a few `HTTP 200` requests, then `HTTP 429` with a
`relay_spend_cap_exceeded` error that reports the cap and the month-to-date
spend.

Keys are secrets. Keep them out of shell history and version control in real
deployments.

## How the price is chosen

The cap is compared against the prices in the Relay catalog, not the provider
invoice. The image ships `config/catalog.json`, which prices
`tensorzero::function_name::chat` and `tensorzero::model_name::gpt-4o-mini` at
0.15 USD per million input tokens and 0.60 USD per million output tokens. See
[`../price-catalog`](../price-catalog) to publish your own prices.

Metered startup fails closed: every model and function in `tensorzero.toml`
must have an enabled catalog price, so no request is ever recorded at a guessed
price.

## Going further

- `RELAY_COST_FEED_URL` delivers each cost event to your own endpoint (JSON,
  idempotent by event id). With `RELAY_COST_FEED_FORMAT=rago` events are sent
  as Lago-compatible usage events for invoicing.
- The Relay UI has a Spend Caps page for the same API.
- Prepaid, success-only billing (`RELAY_BILLING_ENABLED=1`) is a separate mode
  that holds funds before each call. See the main README.

## Clean up

```bash
compose down -v   # -v also deletes the Postgres volume
```
