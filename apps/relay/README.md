# Relay

Relay is an open-source LLM gateway and the maintained fork of
[TensorZero](https://github.com/tensorzero/tensorzero) 2026.6.0. TensorZero
archived its repository on 2026-06-12. Relay keeps the same `gateway` binary,
the same `tensorzero.toml` config, the same `/inference` and `/openai/v1/*`
endpoints and the same `TENSORZERO_*` variables, and keeps shipping engine
fixes, provider and model updates, and security patches.

On top of the TensorZero engine, Relay adds (all opt-in):

- **Budgets.** Per-API-key cost recording and monthly spend caps that stop a
  key with `429` before any provider is called.
- **Price catalog.** One price list for quotes and for metering, with exact
  integer micro-USD math and optional list-price refresh from OpenRouter.
- **Fallback routing.** Per-provider retries, provider failover, fallback
  variants across model families, and cost-bounded latency hedging with
  circuit breaking.
- **Operator UI.** Provider keys stored encrypted, a price editor, spend by key
  and model, and spend caps.
- **Prepaid billing.** Success-only charging (hold, then capture exact usage)
  and AITP-402 pay-per-call challenges for agents.

Relay is free to use under the Apache License 2.0. It is an independent
project, not affiliated with or endorsed by TensorZero or its authors.

## Quickstart

You need Docker and one provider key.

```bash
git clone <this repository> relay && cd relay
export OPENAI_API_KEY=sk-...
docker compose -f docker-compose.quickstart.yml up --build
```

The first build compiles the gateway from source and takes several minutes.
Then, in another terminal:

```bash
curl http://localhost:3000/openai/v1/chat/completions \
  -H "Content-Type: application/json" \
  -d '{
    "model": "tensorzero::function_name::chat",
    "messages": [{"role": "user", "content": "Say hello in five words."}]
  }'
```

Or use any OpenAI SDK by changing the base URL:

```python
from openai import OpenAI

client = OpenAI(base_url="http://localhost:3000/openai/v1", api_key="relay-quickstart")
reply = client.chat.completions.create(
    model="tensorzero::function_name::chat",
    messages=[{"role": "user", "content": "Say hello in five words."}],
)
print(reply.choices[0].message.content)
```

The quickstart runs one stateless gateway with
[`config/quickstart/tensorzero.toml`](config/quickstart/tensorzero.toml): no
database and no auth, for local development only. Set `OPENROUTER_API_KEY` as
well and a failed OpenAI call is retried on OpenRouter.

## Examples

| Example | Shows |
| --- | --- |
| [`examples/relay/openai-client`](examples/relay/openai-client) | OpenAI Python and Node SDKs and curl: functions, named models, streaming, embeddings. |
| [`examples/relay/fallback-routing`](examples/relay/fallback-routing) | Retries, provider failover, fallback variants, latency hedging. |
| [`examples/relay/budgets`](examples/relay/budgets) | API keys, cost recording and per-key monthly spend caps (adds Postgres). |
| [`examples/relay/price-catalog`](examples/relay/price-catalog) | Publishing your own prices and getting exact quotes. |

## Documentation

- Coming from TensorZero: [`docs/MIGRATING-FROM-TENSORZERO.md`](docs/MIGRATING-FROM-TENSORZERO.md).
  An existing config runs unchanged.
- Gateway configuration reference: [`docs/`](docs) (inherited from TensorZero
  and still accurate for the engine).
- What changed since the fork: [`CHANGELOG.md`](CHANGELOG.md).
- Production: the [`Dockerfile`](Dockerfile) and
  [`config/tensorzero.toml`](config/tensorzero.toml), described under
  [Production container](#production-container) below.
- Attribution: [`NOTICE`](NOTICE) and [`LICENSE`](LICENSE) (Apache-2.0).
  Upstream files keep their notices and history. Code boundary:
  [`OWNERSHIP.md`](OWNERSHIP.md).

## Relay product integration

This directory vendors TensorZero as Railyard's standalone Relay inference
gateway. Its executable is `gateway` in the `crates` workspace. A no-model
development instance starts with:

```bash
cd apps/relay/crates
cargo run -p gateway -- --default-config
```

Pass `--config-file <path-or-glob>` for real provider/model configuration. The
gateway configuration reference lives in [`docs/`](docs) (see
[Engine features](#engine-features) below).

The checked-in server-side production baseline is `config/tensorzero.toml`. It
enables Postgres observability and Cloudflare R2 object storage using
environment references only. From the repository root, validate the shared
cloud profile and run it without exposing values:

```bash
python3 scripts/cloud-runtime-env.py relay --check
cd apps/relay/crates
python3 ../../../scripts/cloud-runtime-env.py relay -- \
  cargo run -p gateway -- --config-file ../config/tensorzero.toml
```

Relay adds opt-in success-only Mint billing. When
`RELAY_BILLING_ENABLED=1`, startup now fails closed unless `MINT_SERVER_URL`,
`MINT_API_KEY`, `MINT_PAYEE_ACCOUNT`, `RELAY_AITP_PAYEE`, authenticated gateway
access, Postgres, and `MINT_VERIFICATION_KEYS_JSON` are configured; it no longer
silently falls back to an unfunded in-memory ledger. Verification keys are a JSON
array of `{ "key_id": "...", "public_key_hex": "<64 lowercase hex>" }` and are
operator-pinned rather than dynamically trusted from Mint discovery.
`MINT_PAYER_ACCOUNT` is an optional single-payer override.

Relay persists the exact reservation and one immutable terminal command in
Postgres before Mint calls, recovers stale commands without rerunning inference,
and atomically stores verified capture evidence with command completion.
Periodic recovery, panic/disconnect refunds, and the final shutdown recovery
pass run in TensorZero's tracked task set, so graceful shutdown waits for their
durable commands instead of abandoning unowned background tasks.
Execution-owner fencing plus a renewable stream lease prevents duplicate callers
or stale recovery workers from releasing a live hold. The signed authorization's
full held amount is persisted separately from Relay's estimate, direct recovery
reuses the already-resolved payer and idempotency key, and startup verifies that
`RELAY_AITP_PAYEE` resolves in Mint to `MINT_PAYEE_ACCOUNT`. Streaming terminal
usage/`[DONE]` is withheld until capture intent is durable; incomplete usage
fails closed and is never converted to a guessed charge. The embedded model
catalog can be overlaid with the validated legacy `RELAY_PRICE_CATALOG_PATH`,
or replaced with a full capability-aware snapshot through
`RELAY_CATALOG_PATH` (the two are mutually exclusive). The exact same catalog
instance drives billing and the authenticated `GET /v1/relay/catalog` and
`POST /v1/relay/quotes` endpoints, so apps cannot see a price different from
the configured money path. Unknown models retain the conservative billing
default.
Native `/inference` `model_name` and `function_name` values, OpenAI-compatible
TensorZero selectors, and response variant selectors all canonicalize to stable
catalog identities, so a function-specific override does not change when
TensorZero selects a different variant. Billing covers native inference,
OpenAI-compatible chat/completions/responses, and embeddings. Native
`params.chat_completion.max_tokens` participates in the authorization ceiling,
and embedding responses settle prompt usage with an explicit zero output
dimension. Request pricing accepts the same 100 MiB ceiling as the gateway, so
large multimodal JSON is not misclassified as an unknown maximum-cost request.
Paid batch inference uses one durable hold across its asynchronous lifecycle:
the accepted batch ID and function billing identity are committed before the
start response is exposed, pending aggregate polls retain the hold, provider
failure releases it, and a completed aggregate poll sums every inference's
normalized usage and captures exactly once. Batch ownership is principal-bound
and a leased single-poller transition prevents concurrent reconciliation.
Paid callers must poll the whole batch; the per-inference poll route is rejected
because a partial response cannot prove the aggregate charge for the one hold.
Named TensorZero models and functions are operator-controlled retail routes:
they never use the generic fallback. Paid startup verifies that every configured
model, embedding model, and non-internal function has an enabled price in the
effective catalog. `/health` also probes that catalog and, when billing is
enabled, performs a protected non-mutating Mint ledger read so revoked Mint
credentials or a failed billing store make Relay unready.

## Production container

`Dockerfile` builds the production gateway image. The build context is this
directory (`apps/relay/`): a multi-stage Rust builder compiles
`cargo build -p gateway --release` against the `crates/` workspace, and a
`debian:trixie-slim` runtime carries only CA certificates, the compiled
`gateway` binary, and the checked-in `config/` directory. The entrypoint runs
`gateway --config-file /app/config/tensorzero.toml` and the container listens on
port `3000` (matching `[gateway] bind_address` in `config/tensorzero.toml`).

```bash
# Built from the apps/relay directory (build context = .)
docker build -t railyard-relay -f Dockerfile .
docker run -p 3000:3000 --env-file relay.env railyard-relay
```

The image ships a default retail catalog at `config/catalog.json` and defaults
`RELAY_CATALOG_PATH=/app/config/catalog.json`. That catalog is a
`relay-catalog` `CatalogSnapshot` (`schema_version: 1`) whose enabled entries
price every model, embedding model, and non-internal function configured in
`config/tensorzero.toml` (`chat`, `passport_text`, `gpt-4o-mini`, `gpt-4o`,
`claude-sonnet`, `text-embedding-3-small`), so paid startup route validation
passes. Override `RELAY_CATALOG_PATH` (or bind-mount a different file) to publish
your own operator prices. All provider credentials and the billing variables
below are supplied at runtime as environment variables — no secret is ever baked
into the image or the config.

### Fail-closed billing environment

Success-only Mint billing is opt-in and **fails closed**: when
`RELAY_BILLING_ENABLED` is truthy the gateway aborts startup unless every
required variable below is present and consistent, rather than silently falling
back to an unfunded in-memory ledger. Leave `RELAY_BILLING_ENABLED` unset (the
default) to run the gateway with billing disabled.

| Variable | Required when billing enabled | Purpose |
| --- | --- | --- |
| `RELAY_BILLING_ENABLED` | — (the switch) | Truthy (`1`/`true`/`yes`) turns on success-only Mint billing. When unset, billing is off and the other variables are ignored. |
| `MINT_SERVER_URL` | Yes | Base URL of the Mint server that holds and captures funds. Startup aborts if unset. |
| `MINT_API_KEY` | Yes | API key Relay uses to authenticate to the Mint server. Startup aborts if unset. |
| `MINT_PAYEE_ACCOUNT` | Yes | Mint account that receives captured charges. Startup verifies `RELAY_AITP_PAYEE` resolves to this account in Mint. |
| `RELAY_AITP_PAYEE` | Yes | AITP payee identity advertised to clients; must map to `MINT_PAYEE_ACCOUNT`. Startup aborts if unset or mismatched. |
| `RELAY_CATALOG_PATH` | Recommended | Absolute path to the operator price catalog JSON (defaults to `/app/config/catalog.json` in the image). Mutually exclusive with the legacy `RELAY_PRICE_CATALOG_PATH`. |

### Keeping list prices current

Every row priced from a provider list (`"source": "provider_list"`) can be
refreshed from OpenRouter's public model list. The list is public and needs no
key. This is opt-in:

| Variable | Default | Purpose |
| --- | --- | --- |
| `RELAY_CATALOG_REFRESH_URL` | unset (off) | `openrouter` (or any `https://` mirror of `GET /api/v1/models`) turns the refresh on. |
| `RELAY_CATALOG_REFRESH_INTERVAL_SECS` | `3600` | Seconds between refreshes. The minimum is `300`. The first refresh runs in the background right after startup. |
| `RELAY_CATALOG_REFRESH_ADD_MODELS` | off | When truthy, feed models that are not in the deployed catalog are listed too. When off, only existing rows are repriced. |

The refresh fails closed. If the feed is unreachable, does not parse, is
truncated, or has too many unusable rows, Relay keeps the last good prices. A
paid model that suddenly reads as free keeps its old price. Operator overrides,
contract prices and TensorZero function or named-model routes are never
changed. Operator edits made through the price editor are applied again after
every refresh, so they still take precedence.

Additional related variables: `MINT_VERIFICATION_KEYS_JSON` (operator-pinned
Ed25519 verification keys, also required when billing is enabled),
`MINT_PAYER_ACCOUNT` (optional single-payer override), and
`RELAY_PRICE_CATALOG_PATH` (legacy price overlay, mutually exclusive with
`RELAY_CATALOG_PATH`). Billing additionally requires authenticated gateway
access and Postgres for durable settlement evidence.

The Relay-owned integration surface is split into small crates under
`crates/` so other Railyard applications never depend on TensorZero gateway
internals:

- `relay-contract`: versioned wire types, exact microdollars, model selectors,
  normalized usage, AITP headers/challenges, errors, and settlement evidence.
- `relay-sdk`: hardened async Rust client for native and OpenAI-compatible JSON,
  streaming SSE, embeddings, batches, multipart uploads, typed 402 challenges,
  paid retries across non-streaming and streaming inference, typed unhealthy
  dependency snapshots, catalog/quote discovery, and settlement lookup. Dropping a paid
  stream before `[DONE]` can enqueue its request ID without network I/O; the
  separately-run recovery worker performs the asynchronous evidence lookup.
  Settled replays fail closed unless a caller-pinned
  `MintAttestationValidator` independently verifies the Mint signature,
  receipt projection, and principal/request binding.
- `clients/relay-js`: dependency-free JavaScript/TypeScript client for Node,
  browsers, Expo, and React Native. It mirrors the paid one-retry flow,
  catalog/quote discovery, typed health, settlement evidence, binary multipart,
  bounded SSE, and principal-scoped cost-event operations needed by web and
  mobile apps.
  Replay evidence requires an injected pinned-key verifier before it becomes a
  trusted already-settled result; absent or rejected verification has distinct
  fail-closed errors. Python and Ruby enforce the same boundary.
- `clients/relay-python`: dependency-free synchronous and asynchronous Python
  client for backend services. It uses injected transports or a redirect-free
  standard-library transport and mirrors Relay's paid, streaming, discovery,
  evidence, binary multipart, and cost-event boundaries for Menivor's FastAPI layer.
- `clients/relay-ruby`: dependency-free Ruby client with the same typed AITP
  one-retry boundary, paid streaming, binary multipart, live discovery/quotes,
  settlement evidence, and principal-scoped cost-event operations.
- `relay-catalog`: refreshable model discovery, capability metadata, exact
  quotes, last-good snapshot refresh, and the adapter used by gateway billing.
- `relay-aitp`: durable challenge ownership and replay state. Challenges are
  bound to the authenticated Relay principal; another account cannot reuse a
  payer-signed authorization even if it obtains the request ID and headers.
  Its Mint validator checks the operator-pinned Ed25519 key, signed payload,
  receipt projection, and canonical principal/request reference. Bounded
  retention cleanup removes stale challenges without touching live ones.
- `relay-cost-feed`: idempotent cost-event contract, durable leasing, retry and
  dead-letter policy. The gateway inserts each captured cost event in the same
  Postgres transaction that commits Mint evidence and billing completion.
  Authenticated owners can list `GET /v1/billing/cost-events` with `state`,
  `before`, and bounded `limit` filters, inspect a specific event, and requeue
  only their own dead letters through the matching `/replay` endpoint;
  published or live events reject replay.
- `relay-storage-postgres`: independently buildable restart-safe billing
  coordinator, challenge/evidence/cost-event/provider-health adapters, and the
  ordered Relay-only migration entry point. Paid gateway startup applies that
  schema transactionally under a Postgres advisory lock and records each
  migration once. The crate deliberately avoids the
  TensorZero gateway/V8 dependency graph. Its credential-free recovery harness
  injects a Mint capture outage and proves that the hold remains authoritative,
  only one replacement worker replays it, and actual usage is debited once.
- `relay-billing` and `relay-hedge`: success-only Mint settlement and adaptive,
  cost-bounded provider execution.

Cost events are never dropped when a consumer is unavailable. If
`RELAY_COST_FEED_URL` is set, the gateway delivers them as JSON with the event
UUID in `Idempotency-Key`; `RELAY_COST_FEED_BEARER` optionally authenticates the
request. Without a URL, rows remain pending in `relay_cost_events`. Delivery
uses bounded HTTP timeouts, disables redirects, leases rows with `SKIP LOCKED`,
backs off failed attempts, and dead-letters after the configured attempt limit.

### Postpaid metering, Rago invoicing, and spend caps

Consumers that invoice their own tenants (for example AutoGTM via Rago) run
Relay in **postpaid meter-only** mode instead of prepaid Mint billing:

| Variable | Purpose |
| --- | --- |
| `RELAY_METERING_ENABLED` | Truthy records every successful authenticated inference as a durable `metered:` cost event priced from the catalog, with no AITP challenge or Mint hold. Requires `[gateway.auth]` and Postgres (startup fails closed otherwise). Ignored when `RELAY_BILLING_ENABLED` is set, which already emits captured events. |
| `RELAY_COST_FEED_FORMAT` | `raw` (default, Relay `CostEvent` JSON) or `rago`. |
| `RELAY_COST_FEED_URL` | Delivery endpoint. For `rago`, the Rago origin (e.g. `https://rago.internal`); `/api/v1/events` is appended. |
| `RELAY_COST_FEED_BEARER` | Bearer token; for `rago` this is the Rago API key (required). |
| `RELAY_RAGO_METRIC_CODE` | Rago billable metric code (default `relay_llm_usage`). Configure it as a `sum_agg` over `charged_micros` (retail spend) or `total_tokens`. |
| `RELAY_RAGO_SUBSCRIPTION_PREFIX` | Prefix for the Rago `external_subscription_id`, which defaults to `<prefix><api key public id>`. |
| `RELAY_RAGO_SUBSCRIPTION_MAP` | Optional JSON object mapping API-key public ids to explicit Rago subscription ids. |
| `RELAY_RAGO_ROUTES_JSON` | Optional multi-org routing: JSON object mapping an API-key public id to `{"api_key_env": "<env var holding that org's Rago key>", "subscription_id": "..."}` (or an inline `"api_key"`). Routed keys bill into their own Rago org; unrouted keys use `RELAY_COST_FEED_BEARER` and the map/prefix above. With routes set, `RELAY_COST_FEED_BEARER` is optional, and an unrouted key without it is retried and then dead-lettered instead of being billed to another org. |
| `RELAY_OPERATOR_KEY_IDS` | Comma-separated API-key public ids allowed to aggregate spend across all keys (`?scope=all`), replay any dead letter, and manage spend caps. List the UI's `TENSORZERO_API_KEY` here. |

In `rago` format each event is posted as a Lago-compatible usage event whose
`transaction_id` is the Relay event id, so redelivery is idempotent (Rago's
duplicate-transaction 422 is treated as delivered). Properties carry the model,
route, token counts, `charged_micros`, and `charged_usd`.

Per-key monthly spend caps live in `relay_key_budgets` and are managed from the
UI's **Spend Caps** page or `PUT/DELETE /relay/billing/budgets/{key_id}`. When a
key's month-to-date (UTC) charged spend reaches its cap, billable requests get
HTTP 429 `relay_spend_cap_exceeded` before any provider or Mint I/O, in both
billing modes. `GET /relay/billing/status` reports the active mode, feed target,
and any configuration gaps; `GET /v1/relay/provider_health` reports routing and
circuit-breaker state per model.

Provider batches (`POST /batch_inference`) are metered too: the start records
the owning key in `relay_metered_batches`, and the first poll that sees the
batch `completed` records one cost event per inference (keyed by inference id,
so repeated polls never double-bill). A postpaid stream the client aborts before
its terminal usage frame is recorded from an estimate (request bytes / 4 and
streamed characters / 4 tokens).

### Operator surfaces: wallets, prices, provider keys

| Variable | Purpose |
| --- | --- |
| `RELAY_TOPUP_PROVIDERS` | Comma-separated Mint checkout providers (`creem`, `stripe`, `polar`, `paypal`, ...) offered for self-serve wallet top-ups on the Billing page (`POST /relay/billing/wallet/top_up`). Prepaid mode only; each provider must also be active in Mint. |
| `RELAY_WALLET_TREASURY_ACCOUNT` | Funded Mint account operator credits are drawn from (`POST /relay/billing/wallet/credit`, operator keys only, idempotent per submission). |
| `RELAY_CREDENTIALS_KEY` | 32 random bytes (base64 or 64 hex chars) sealing provider API keys stored from the Providers page with AES-256-GCM. Invalid values abort startup; unset leaves key management read-only (environment keys only). Generate with `openssl rand -base64 32`. |
| `OPENROUTER_API_KEY` | Optional environment key for the OpenRouter failover leg every model routes to after its primary provider. |

Retail prices are edited on the Models page (`PUT /v1/relay/catalog/prices`,
`POST /v1/relay/catalog/prices/reset`, operator keys only). Edits persist in
`relay_catalog_price_overrides`, overlay the deployed catalog, and reprice
billing and metering immediately on the replica that saved them and within 30
seconds on every other replica. Provider keys stored on the Providers page
(`/v1/relay/provider_keys`) are supplied to TensorZero as `dynamic::relay_*`
credentials and take precedence over the environment variables named in
`config/tensorzero.toml`; the API only ever returns a four-character hint.

The current dependency and completion graph is maintained in
[`docs/RELAY-ENGINEERING-GRAPH.md`](docs/RELAY-ENGINEERING-GRAPH.md).
The exact code-complete versus live-infrastructure boundary is recorded in
[`docs/RELAY-RELEASE-BOUNDARY.md`](docs/RELAY-RELEASE-BOUNDARY.md).

The Relay-only Postgres lifecycle can be checked without Docker, provider
credentials, or Mint credentials:

```bash
python3 tools/relay_postgres_smoke.py
```

The runner creates an ephemeral local Postgres cluster, applies the six Relay
migrations, and verifies principal-bound challenges, atomic
settlement/evidence/cost-event commits, idempotency, concurrent `SKIP LOCKED`
leases, batch metadata constraints, and provider-health checkpoints. It needs a
host that permits Postgres shared memory; restricted sandboxes that reject
`shmget` cannot start the database process.

Relay also adds opt-in, per-model provider hedging and adaptive SLO routing on
top of TensorZero's provider dispatch. It is disabled by default. When enabled,
non-streaming calls race to the first complete success and streaming calls race
to the first token; losing futures are cancelled and only the winning response
can reach success-only billing. Real attempt latency, TTFT, retryable failures,
and fatal failures feed the per-provider EWMA/circuit-breaker ranker.
Prometheus metrics expose `relay_provider_attempts_total`, provider latency and
TTFT histograms, hedge request outcomes, launched-attempt width, and per-provider
wins.

When Postgres is configured, the gateway restores adaptive provider-health
records before serving, checkpoints them every 30 seconds, and performs a final
tracked write on graceful shutdown. Snapshots preserve latency/error EWMAs and
circuit state while subtracting process downtime from an open circuit's
remaining cooldown. Malformed records are ignored and persistence failure falls
back to neutral scores because routing health is an optimization, never billing
or request authority.

```toml
[models.chat.hedge]
enabled = true
max_concurrency = 2
stagger_ms = 75
max_cost_micros = 120000

[models.chat.hedge.provider_cost_micros]
primary = 60000
fallback = 55000
```

Secondary launches use progressive absolute deadlines (`1×`, `2×`, … the
configured `stagger_ms`) rather than bursting together. With
`max_cost_micros` set, a secondary provider without an explicit
`provider_cost_micros` entry is conservatively blocked from the concurrent
window. The primary still runs, so incomplete cost metadata degrades to normal
fallback instead of making the request unavailable or silently exceeding the
cap. Hedge configuration survives TensorZero stored-config snapshots.

The exact owned/upstream boundary and the rules that keep Relay independently
operable are documented in [`OWNERSHIP.md`](OWNERSHIP.md).

## Engine features

These capabilities come from the TensorZero engine that Relay is built on. The
reference pages live under [`docs/`](docs) in this repository (MDX sources
inherited from upstream).

- **Gateway.** One OpenAI-compatible API in front of Anthropic, AWS Bedrock,
  AWS SageMaker, Azure, DeepSeek, Fireworks, GCP Vertex AI (Anthropic and
  Gemini), Google AI Studio, Groq, Hyperbolic, Mistral, OpenAI, OpenRouter,
  SGLang, TGI, Together AI, vLLM, xAI and any OpenAI-compatible server
  (for example Ollama). Tool use, JSON outputs, batch, embeddings, images and
  files, caching, retries, fallbacks and timeouts.
  See [`docs/gateway`](docs/gateway).
- **Prompt templates and schemas** that give each function a typed interface.
- **Observability.** Inferences and feedback stored in your own database,
  OpenTelemetry trace export and Prometheus metrics.
  See [`docs/observability`](docs/observability) and
  [`docs/operations`](docs/operations).
- **Experimentation.** Weighted and adaptive variant selection.
  See [`docs/experimentation`](docs/experimentation).
- **Datasets and inference evaluations** that run on your own gateway and
  database. See [`docs/evaluations`](docs/evaluations).

### Not available in Relay

**Autopilot**, upstream's automated AI engineer, was a paid product that ran
on TensorZero's hosted service (`api.autopilot.tensorzero.com`). That service
is not part of this repository and Relay does not offer it. The operator UI
hides every Autopilot entry point, and a direct visit to `/autopilot` shows a
"not available in Relay" page.

Supervised fine-tuning and evaluations do not use any upstream-hosted service
and remain available. Fine-tuning jobs run on the provider you pick (OpenAI,
Fireworks, GCP Vertex AI Gemini or Together AI) with your own provider
credentials, and are billed by that provider. Evaluations run on your own
gateway and database.

## Attribution and license

Relay is a fork of [TensorZero](https://github.com/tensorzero/tensorzero),
taken at the upstream release tag `2026.6.0` (the last release before the
upstream repository was archived on 2026-06-12). TensorZero is licensed under
the Apache License 2.0, and so is Relay: see [`LICENSE`](LICENSE) and
[`NOTICE`](NOTICE). Upstream source files keep their original copyright and
license notices; Relay's changes are recorded in the git history and in
[`CHANGELOG.md`](CHANGELOG.md).

Relay is an independent project. It is not affiliated with, sponsored by, or
endorsed by TensorZero or its authors, and TensorZero's support channels do
not cover Relay. The name "TensorZero" appears only to identify the upstream
project, and in compatibility identifiers (`tensorzero.toml`, `TENSORZERO_*`
variables, the `tensorzero::` model prefix) that Relay keeps so existing
deployments run unchanged.
