# Relay product integration

This directory vendors TensorZero as Railyard's standalone Relay inference
gateway. Its executable is `gateway` in the `crates` workspace. A no-model
development instance starts with:

```bash
cd apps/relay/crates
cargo run -p gateway -- --default-config
```

Pass `--config-file <path-or-glob>` for real provider/model configuration. The
upstream documentation below remains the authoritative gateway configuration
reference.

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

This subtree is a maintained vendor fork, so upstream claims and links below
describe TensorZero itself, not automatically the Railyard Relay deployment.

The exact owned/upstream boundary and the rules that keep Relay independently
operable are documented in [`OWNERSHIP.md`](OWNERSHIP.md).

---

<p><picture><img src="https://github.com/user-attachments/assets/9d0a93c6-7685-4e57-9737-7cbeb338a218" alt="TensorZero Logo" width="128" height="128"></picture></p>

# TensorZero

<p><picture><img src="https://www.tensorzero.com/github-trending-badge.svg" alt="GitHub Trending - #1 Repository Of The Day"></picture></p>

**TensorZero is an open-source LLMOps platform that unifies:**

- **Gateway:** access every LLM provider through a unified API, built for performance (<1ms p99 latency)
- **Observability:** store inferences and feedback in your database, available programmatically or in the UI
- **Evaluation:** benchmark individual inferences or end-to-end workflows using heuristics, LLM judges, etc.
- **Optimization:** collect metrics and human feedback to optimize prompts, models, and inference strategies
- **Experimentation:** ship with confidence with built-in A/B testing, routing, fallbacks, retries, etc.

You can take what you need, adopt incrementally, and complement with other tools.
It plays nicely with the **OpenAI SDK**, **OpenTelemetry**, and **every major LLM provider**.

TensorZero is used by companies ranging from frontier AI startups to the Fortune 10 and fuels ~1% of global LLM API spend today.

<br>

<p align="center">
  <b><a href="https://www.tensorzero.com/" target="_blank">Website</a></b>
  ·
  <b><a href="https://www.tensorzero.com/docs" target="_blank">Docs</a></b>
  ·
  <b><a href="https://www.x.com/tensorzero" target="_blank">Twitter</a></b>
  ·
  <b><a href="https://www.tensorzero.com/slack" target="_blank">Slack</a></b>
  ·
  <b><a href="https://www.tensorzero.com/discord" target="_blank">Discord</a></b>
  <br>
  <br>
  <b><a href="https://www.tensorzero.com/docs/quickstart" target="_blank">Quick Start (5min)</a></b>
  ·
  <b><a href="https://www.tensorzero.com/docs/deployment/tensorzero-gateway" target="_blank">Deployment Guide</a></b>
  ·
  <b><a href="https://www.tensorzero.com/docs/gateway/api-reference" target="_blank">API Reference</a></b>
  ·
  <b><a href="https://www.tensorzero.com/docs/gateway/configuration-reference" target="_blank">Configuration Reference</a></b>
</p>

## Demo

<video src="https://github.com/user-attachments/assets/04a8466e-27d8-4189-b305-e7cecb6881ee"></video>

## Features

> [!NOTE]
>
> ### 🆕 TensorZero Autopilot
>
> TensorZero Autopilot is an **automated AI engineer** powered by TensorZero that analyzes LLM observability data, sets up evals, optimizes prompts and models, and runs A/B tests.
>
> It **dramatically improves the performance of LLM agents** across diverse tasks:
>
> <img width="600" alt="Bar chart showing baseline vs. optimized scores across diverse LLM tasks" src="https://github.com/user-attachments/assets/aa474fe3-b55a-48aa-9f0d-e7c2f8e32ccd" />
> <br>
>
> **[Learn more →](https://www.tensorzero.com/blog/automated-ai-engineer/)**

### 🌐 LLM Gateway

> **Integrate with TensorZero once and access every major LLM provider.**

- [x] **[Call any LLM](https://www.tensorzero.com/docs/gateway/call-any-llm)** (API or self-hosted) through a single unified API
- [x] Infer with **[tool use](https://www.tensorzero.com/docs/gateway/guides/tool-use)**, **[structured outputs (JSON)](https://www.tensorzero.com/docs/gateway/generate-structured-outputs)**, **[batch](https://www.tensorzero.com/docs/gateway/guides/batch-inference)**, **[embeddings](https://www.tensorzero.com/docs/gateway/generate-embeddings)**, **[multimodal (images, files)](https://www.tensorzero.com/docs/gateway/call-llms-with-image-and-file-inputs)**, **[caching](https://www.tensorzero.com/docs/gateway/guides/inference-caching)**, etc.
- [x] **[Create prompt templates and schemas](https://www.tensorzero.com/docs/gateway/create-a-prompt-template)** to enforce a structured interface between your application and the LLMs
- [x] Satisfy extreme throughput and latency needs, thanks to 🦀 Rust: **[<1ms p99 latency overhead at 10k+ QPS](https://www.tensorzero.com/docs/gateway/benchmarks)**
- [x] **[Ensure high availability](https://www.tensorzero.com/docs/gateway/guides/retries-fallbacks)** with routing, retries, fallbacks, load balancing, granular timeouts, etc.
- [x] **[Track usage and cost](https://www.tensorzero.com/docs/operations/track-usage-and-cost)** and **[enforce custom rate limits](https://www.tensorzero.com/docs/operations/enforce-custom-rate-limits)** with granular scopes (e.g. tags)
- [x] **[Set up auth for TensorZero](https://www.tensorzero.com/docs/operations/set-up-auth-for-tensorzero)** to allow clients to access models without sharing provider API keys

#### Supported Model Providers

**[Anthropic](https://www.tensorzero.com/docs/gateway/guides/providers/anthropic)**,
**[AWS Bedrock](https://www.tensorzero.com/docs/gateway/guides/providers/aws-bedrock)**,
**[AWS SageMaker](https://www.tensorzero.com/docs/gateway/guides/providers/aws-sagemaker)**,
**[Azure](https://www.tensorzero.com/docs/gateway/guides/providers/azure)**,
**[DeepSeek](https://www.tensorzero.com/docs/gateway/guides/providers/deepseek)**,
**[Fireworks](https://www.tensorzero.com/docs/gateway/guides/providers/fireworks)**,
**[GCP Vertex AI Anthropic](https://www.tensorzero.com/docs/gateway/guides/providers/gcp-vertex-ai-anthropic)**,
**[GCP Vertex AI Gemini](https://www.tensorzero.com/docs/gateway/guides/providers/gcp-vertex-ai-gemini)**,
**[Google AI Studio (Gemini API)](https://www.tensorzero.com/docs/gateway/guides/providers/google-ai-studio-gemini)**,
**[Groq](https://www.tensorzero.com/docs/gateway/guides/providers/groq)**,
**[Hyperbolic](https://www.tensorzero.com/docs/gateway/guides/providers/hyperbolic)**,
**[Mistral](https://www.tensorzero.com/docs/gateway/guides/providers/mistral)**,
**[OpenAI](https://www.tensorzero.com/docs/gateway/guides/providers/openai)**,
**[OpenRouter](https://www.tensorzero.com/docs/gateway/guides/providers/openrouter)**,
**[SGLang](https://www.tensorzero.com/docs/gateway/guides/providers/sglang)**,
**[TGI](https://www.tensorzero.com/docs/gateway/guides/providers/tgi)**,
**[Together AI](https://www.tensorzero.com/docs/gateway/guides/providers/together)**,
**[vLLM](https://www.tensorzero.com/docs/gateway/guides/providers/vllm)**, and
**[xAI (Grok)](https://www.tensorzero.com/docs/gateway/guides/providers/xai)**.

Need something else? TensorZero also supports **[any OpenAI-compatible API (e.g. Ollama)](https://www.tensorzero.com/docs/gateway/guides/providers/openai-compatible)**.

#### Usage Example

You can use TensorZero with any OpenAI SDK (Python, Node, Go, etc.) or OpenAI-compatible client.

1. **[Deploy the TensorZero Gateway](https://www.tensorzero.com/docs/deployment/tensorzero-gateway)** (one Docker container).
2. Update the `base_url` and `model` in your OpenAI-compatible client.
3. Run inference:

```python
from openai import OpenAI

# Point the client to the TensorZero Gateway
client = OpenAI(base_url="http://localhost:3000/openai/v1", api_key="not-used")

response = client.chat.completions.create(
    # Call any model provider (or TensorZero function)
    model="tensorzero::model_name::anthropic::claude-sonnet-4-6",
    messages=[
        {
            "role": "user",
            "content": "Share a fun fact about TensorZero.",
        }
    ],
)
```

See **[Quick Start](https://www.tensorzero.com/docs/quickstart)** for more information.

### 🔍 LLM Observability

> **Zoom in to debug individual API calls, or zoom out to monitor metrics across models and prompts over time &mdash; all using the open-source TensorZero UI.**

- [x] Store inferences and **[feedback (metrics, human edits, etc.)](https://www.tensorzero.com/docs/gateway/guides/metrics-feedback)** in your own database
- [x] Dive into individual inferences or high-level aggregate patterns using the TensorZero UI or programmatically
- [x] **[Build datasets](https://www.tensorzero.com/docs/gateway/api-reference/datasets-datapoints)** for optimization, evaluation, and other workflows
- [x] Replay historical inferences with new prompts, models, inference strategies, etc.
- [x] **[Export OpenTelemetry traces (OTLP)](https://www.tensorzero.com/docs/operations/export-opentelemetry-traces)** and **[export Prometheus metrics](https://www.tensorzero.com/docs/operations/export-prometheus-metrics)** to your favorite application observability tools
- [ ] Soon: AI-assisted debugging and root cause analysis; AI-assisted data labeling

### 📈 LLM Optimization

> **Send production metrics and human feedback to easily optimize your prompts, models, and inference strategies &mdash; using the UI or programmatically.**

- [x] Optimize your models with **[supervised fine-tuning](https://www.tensorzero.com/docs/optimization/supervised-fine-tuning-sft)**, RLHF, and other techniques
- [x] Optimize your prompts with automated prompt engineering algorithms like **[GEPA](https://www.tensorzero.com/docs/optimization/gepa)**
- [x] Optimize your **[inference strategy](https://www.tensorzero.com/docs/gateway/guides/inference-time-optimizations)** with **[dynamic in-context learning](https://www.tensorzero.com/docs/optimization/dynamic-in-context-learning-dicl)**, best/mixture-of-N sampling, etc.
- [x] Enable a feedback loop for your LLMs: a data & learning flywheel turning production data into smarter, faster, and cheaper models
- [ ] Soon: synthetic data generation

### 📊 LLM Evaluation

> **Compare prompts, models, and inference strategies using evaluations powered by heuristics and LLM judges.**

- [x] **[Evaluate individual inferences](https://www.tensorzero.com/docs/evaluations/inference-evaluations/tutorial)** with _inference evaluations_ powered by heuristics or LLM judges (&approx; unit tests for LLMs)
- [x] **[Evaluate end-to-end workflows](https://www.tensorzero.com/docs/evaluations/workflow-evaluations/tutorial)** with _workflow evaluations_ with complete flexibility (&approx; integration tests for LLMs)
- [x] Optimize LLM judges just like any other TensorZero function to align them to human preferences
- [ ] Soon: more built-in evaluators; headless evaluations

<table>
  <tr></tr> <!-- flip highlight order -->
  <tr>
    <td width="50%" align="center" valign="middle"><b>Evaluation » UI</b></td>
    <td width="50%" align="center" valign="middle"><b>Evaluation » CLI</b></td>
  </tr>
  <tr>
    <td width="50%" align="center" valign="middle"><img src="https://github.com/user-attachments/assets/f4bf54e3-1b63-46c8-be12-2eaabf615699"></td>
    <td width="50%" align="left" valign="middle">
<pre><code class="language-bash">docker compose run --rm evaluations \
  --evaluation-name extract_data \
  --dataset-name hard_test_cases \
  --variant-name gpt_4o \
  --concurrency 5</code></pre>
<pre><code class="language-bash">Run ID: 01961de9-c8a4-7c60-ab8d-15491a9708e4
Number of datapoints: 100
██████████████████████████████████████ 100/100
exact_match: 0.83 ± 0.03 (n=100)
semantic_match: 0.98 ± 0.01 (n=100)
item_count: 7.15 ± 0.39 (n=100)</code></pre>
    </td>
  </tr>
</table>

### 🧪 LLM Experimentation

> **Ship with confidence with built-in A/B testing, routing, fallbacks, retries, etc.**

- [x] **[Run adaptive A/B tests](https://www.tensorzero.com/docs/experimentation/run-adaptive-ab-tests)** to ship with confidence and identify the best prompts and models for your use cases.
- [x] Enforce principled experiments in complex workflows, including support for multi-turn LLM systems, sequential testing, and more.

### & more!

> **Build with an open-source stack well-suited for prototypes but designed from the ground up to support the most complex LLM applications and deployments.**

- [x] Build simple applications or massive deployments with GitOps-friendly orchestration
- [x] **[Extend TensorZero](https://www.tensorzero.com/docs/operations/extend-tensorzero)** with built-in escape hatches, programmatic-first usage, direct database access, and more
- [x] Integrate with third-party tools: specialized observability and evaluations, model providers, agent orchestration frameworks, etc.
- [x] Iterate quickly by experimenting with prompts interactively using the Playground UI

## Frequently Asked Questions

**How is TensorZero different from other LLM frameworks?**

1. TensorZero enables you to optimize complex LLM applications based on production metrics and human feedback.
2. TensorZero supports the needs of industrial-grade LLM applications: low latency, high throughput, type safety, self-hosted, GitOps, customizability, etc.
3. TensorZero unifies the entire LLMOps stack, creating compounding benefits. For example, LLM evaluations can be used for fine-tuning models alongside AI judges.

**Can I use TensorZero with \_\_\_?**

Yes.
Every major programming language is supported.
It plays nicely with the **OpenAI SDK**, **OpenTelemetry**, and **every major LLM provider**.

**Is TensorZero production-ready?**

Yes.
TensorZero is used by companies ranging from frontier AI startups to the Fortune 10 and powers ~1% of the global LLM API spend today.

Here's a case study: **[Automating Code Changelogs at a Large Bank with LLMs](https://www.tensorzero.com/blog/case-study-automating-code-changelogs-at-a-large-bank-with-llms)**

**How much does TensorZero cost?**

TensorZero (LLMOps platform) is 100% self-hosted and open-source.

TensorZero Autopilot (automated AI engineer) is a complementary paid product powered by TensorZero.

**Who is building TensorZero?**

Our technical team includes a former Rust compiler maintainer, machine learning researchers (Stanford, CMU, Oxford, Columbia) with thousands of citations, and the chief product officer of a decacorn startup. We're backed by the same investors as leading open-source projects (e.g. ClickHouse, CockroachDB) and AI labs (e.g. OpenAI, Anthropic). See our **[$7.3M seed round announcement](https://www.tensorzero.com/blog/tensorzero-raises-7-3m-seed-round-to-build-an-open-source-stack-for-industrial-grade-llm-applications/)** and **[coverage from VentureBeat](https://venturebeat.com/ai/tensorzero-nabs-7-3m-seed-to-solve-the-messy-world-of-enterprise-llm-development/)**. We're **[hiring in NYC](https://www.tensorzero.com/jobs)**.

**How do I get started?**

You can adopt TensorZero incrementally. Our **[Quick Start](https://www.tensorzero.com/docs/quickstart)** goes from a vanilla OpenAI wrapper to a production-ready LLM application with observability and fine-tuning in just 5 minutes.

## Get Started

**Start building today.**
The **[Quick Start](https://www.tensorzero.com/docs/quickstart)** shows it's easy to set up an LLM application with TensorZero.

**Questions?**
Ask us on **[Slack](https://www.tensorzero.com/slack)** or **[Discord](https://www.tensorzero.com/discord)**.

**Using TensorZero at work?**
Email us at **[hello@tensorzero.com](mailto:hello@tensorzero.com)** to set up a Slack or Teams channel with your team (free).

## Examples

We are working on a series of **complete runnable examples** illustrating TensorZero's data & learning flywheel.

> **[Optimizing Data Extraction (NER) with TensorZero](https://github.com/tensorzero/tensorzero/tree/main/examples/data-extraction-ner)**
>
> This example shows how to use TensorZero to optimize a data extraction pipeline.
> We demonstrate techniques like fine-tuning and dynamic in-context learning (DICL).
> In the end, an optimized GPT-4o Mini model outperforms GPT-4o on this task &mdash; at a fraction of the cost and latency &mdash; using a small amount of training data.

> **[Agentic RAG — Multi-Hop Question Answering with LLMs](https://github.com/tensorzero/tensorzero/tree/main/examples/rag-retrieval-augmented-generation/simple-agentic-rag/)**
>
> This example shows how to build a multi-hop retrieval agent using TensorZero.
> The agent iteratively searches Wikipedia to gather information, and decides when it has enough context to answer a complex question.

> **[Writing Haikus to Satisfy a Judge with Hidden Preferences](https://github.com/tensorzero/tensorzero/tree/main/examples/haiku-hidden-preferences)**
>
> This example fine-tunes GPT-4o Mini to generate haikus tailored to a specific taste.
> You'll see TensorZero's "data flywheel in a box" in action: better variants leads to better data, and better data leads to better variants.
> You'll see progress by fine-tuning the LLM multiple times.

> **[Image Data Extraction — Multimodal (Vision) Fine-tuning](https://github.com/tensorzero/tensorzero/tree/main/examples/multimodal-vision-finetuning)**
>
> This example shows how to fine-tune multimodal models (VLMs) like GPT-4o to improve their performance on vision-language tasks.
> Specifically, we'll build a system that categorizes document images (screenshots of computer science research papers).

> **[Improving LLM Chess Ability with Best-of-N Sampling](https://github.com/tensorzero/tensorzero/tree/main/examples/chess-puzzles/)**
>
> This example showcases how best-of-N sampling can significantly enhance an LLM's chess-playing abilities by selecting the most promising moves from multiple generated options.

## Blog Posts

We write about LLM engineering on the **[TensorZero Blog](https://www.tensorzero.com/blog)**.
Here are some of our favorite posts:

- **[Bandits in your LLM Gateway: Improve LLM Applications Faster with Adaptive Experimentation (A/B Testing)](https://www.tensorzero.com/blog/bandits-in-your-llm-gateway/)**
- **[Is OpenAI's Reinforcement Fine-Tuning (RFT) Worth It?](https://www.tensorzero.com/blog/is-openai-reinforcement-fine-tuning-rft-worth-it/)**
- **[Distillation with Programmatic Data Curation: Smarter LLMs, 5-30x Cheaper Inference](https://www.tensorzero.com/blog/distillation-programmatic-data-curation-smarter-llms-5-30x-cheaper-inference/)**
- **[From NER to Agents: Does Automated Prompt Engineering Scale to Complex Tasks?](https://www.tensorzero.com/blog/from-ner-to-agents-does-automated-prompt-engineering-scale-to-complex-tasks/)**
