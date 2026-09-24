# Changelog

All notable changes to Relay since it was forked from TensorZero.

Relay started from **TensorZero 2026.6.0**. TensorZero archived its repository
on 2026-06-12. Changes to upstream files are listed here and kept in the git
history. Upstream's own release notes cover everything up to 2026.6.0.

The crate version still reads `2026.6.0` (the fork baseline). The first public
Relay release will set its own version (see
[`docs/RELAY-PUBLIC-RELEASE-CHECKLIST.md`](docs/RELAY-PUBLIC-RELEASE-CHECKLIST.md)).

Compatibility promise for every entry below: an unmodified TensorZero 2026.6.0
config, environment and client keep working. Everything Relay adds is opt-in.

## Unreleased

### Added

- **Quickstart and examples.** `docker-compose.quickstart.yml` runs one gateway
  with one provider key. `config/quickstart/tensorzero.toml` is the matching
  minimal config. New `examples/relay/` covers OpenAI-compatible clients,
  fallback routing, per-key budgets and the price catalog.
- The Docker image now ships `LICENSE` and `NOTICE` under
  `/usr/share/doc/relay/`, as Apache-2.0 requires for redistribution.
- **OpenRouter-sourced list prices.** The built-in catalog now prices 111
  models from OpenRouter's public model list. Opt-in timed refresh
  (`RELAY_CATALOG_REFRESH_URL=openrouter`, `RELAY_CATALOG_REFRESH_INTERVAL_SECS`,
  `RELAY_CATALOG_REFRESH_ADD_MODELS`) reprices `provider_list` rows with
  integer math, rejects truncated or suspicious feeds, keeps the last good
  prices on failure, and reapplies operator price edits after each refresh.
- **Provider keys from the UI.** `/v1/relay/provider_keys` stores provider API
  keys sealed with AES-256-GCM under `RELAY_CREDENTIALS_KEY`. Configs reference
  them as `dynamic::relay_*_api_key` credentials, with the environment variable
  as fallback. Keys can be tested without spending tokens. The API only
  returns a four-character hint.
- **Runtime price editor.** `PUT /v1/relay/catalog/prices` and
  `POST /v1/relay/catalog/prices/reset` (operator keys) persist price edits
  in Postgres. Edits apply to billing and metering at once on the saving
  replica and within 30 seconds everywhere else.
- **Wallet top-up and operator credit** (prepaid mode):
  `POST /relay/billing/wallet/top_up` with providers from
  `RELAY_TOPUP_PROVIDERS`, and `POST /relay/billing/wallet/credit` from
  `RELAY_WALLET_TREASURY_ACCOUNT`.
- **Batch metering.** `POST /batch_inference` records the owning key, and the
  first completed poll records one cost event per inference, idempotent across
  repeated polls.
- **Postpaid metering** (`RELAY_METERING_ENABLED=1`). Every successful
  authenticated inference becomes a priced, durable cost event without a
  payment hold. Requires auth and Postgres, and fails closed without them.
- **Rago / Lago-compatible cost feed** (`RELAY_COST_FEED_FORMAT=rago`). Cost
  events are delivered as usage events with the event id as
  `transaction_id`, so redelivery is idempotent.
- **Per-key monthly spend caps.** `relay_key_budgets` with
  `GET/PUT/DELETE /relay/billing/budgets`. A key at its cap gets `429
  relay_spend_cap_exceeded` before any provider or payment call, in both
  billing modes. Operator keys are listed in `RELAY_OPERATOR_KEY_IDS`.
- **Billing reads.** `GET /relay/billing/summary`, `/relay/billing/settlements`,
  `/relay/billing/keys` and `/relay/billing/status`, scoped to the calling key
  or, for operators, `?scope=all`.
- **Provider health endpoint.** `GET /v1/relay/provider_health` reports
  routing and circuit-breaker state per model.
- **Per-provider retries** before failover: `retries = { num_retries,
  max_delay_s }` on a model, default one retry with exponential backoff and
  jitter. `Retry-After` from OpenAI, Anthropic and Gemini sets the wait,
  capped at `max_delay_s`.
- **SLO tuning** for the provider-health scorer under `[gateway.relay.slo]`.
- **Fallback routing in the production config.** Every model routes to its
  primary provider, then OpenRouter. The `chat` function falls back from
  OpenAI to Anthropic.
- **Relay UI pages:** Billing (wallet, spend by key and model, settlement
  history), Settlements with raw receipts, per-key spend, Models (catalog and
  price editor), Providers, Spend Caps, and CSV usage export.
- **Production container.** A multi-stage `Dockerfile` for the `gateway`
  binary on `debian:trixie-slim` with a default retail catalog
  (`config/catalog.json`) and a health check.
- **Passport text route.** `/v1/passport/generate` and the `passport_text`
  function with provider-pinned variants. Image, voice and video modalities are
  intentionally left unwired and fail closed.
- **Documentation.** `NOTICE`, `OWNERSHIP.md`,
  `docs/MIGRATING-FROM-TENSORZERO.md`, `docs/RELAY-RELEASE-BOUNDARY.md`,
  `docs/RELAY-ENGINEERING-GRAPH.md`.

### Changed

- The UI is rebranded from TensorZero to Relay (logo, titles, package name).
  Wire-level names (`tensorzero::` selectors, `TENSORZERO_*` variables, routes)
  are unchanged on purpose so existing clients keep working.
- Provider-health recording no longer depends on hedging being active on the
  request path: once any model enables hedging, sequential calls also update
  the shared scorer.
- `config/tensorzero.toml` sets `disable_pseudonymous_usage_analytics = true`.
  Relay does not report usage to TensorZero's servers.

### Fixed

- Stale billing docstrings that described a flat default price.

## Relay baseline: TensorZero 2026.6.0 plus the Relay money path

Imported on 2026-08-24 and completed through 2026-09-21.

### Added

- **Relay crates** with no dependency on TensorZero gateway internals:
  `relay-contract` (versioned wire types, exact microdollars),
  `relay-sdk` (Rust client), `relay-catalog` (catalog, capabilities, exact
  quotes), `relay-aitp` and `relay-aitp-wire` (AITP-402 pay-per-call
  challenges bound to the caller), `relay-billing` (success-only settlement on
  Mint), `relay-cost-feed` (idempotent cost events with leasing, retry and
  dead letters), `relay-hedge` (cost-bounded provider hedging and adaptive
  ranking), `relay-storage-postgres` (restart-safe settlement, evidence and
  migrations).
- **Clients:** `clients/relay-js`, `clients/relay-python`,
  `clients/relay-ruby`, dependency-free, with pinned-key settlement
  verification.
- **Success-only billing** (`RELAY_BILLING_ENABLED=1`). Hold before inference,
  capture exact usage after, release on failure or disconnect. Durable outbox,
  crash recovery without re-running inference, and verified settlement
  evidence. Startup fails closed on any missing billing setting.
- **Per-model price catalog** replacing the flat default price, with
  `GET /v1/relay/catalog` and `POST /v1/relay/quotes` driven by the same
  catalog instance as billing.
- **Provider hedging** (`[models.<name>.hedge]`) with staggered launches, a cost
  cap, loser cancellation, EWMA latency and error scoring, and circuit
  breaking. Health checkpoints persist across restarts when Postgres is
  configured.
- **`/health`** includes catalog and billing readiness.
- `tools/relay_postgres_smoke.py`, a credential-free Postgres lifecycle check.
