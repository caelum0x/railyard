# Rago

A faithful, from-scratch **Rust** rewrite of [Lago](https://github.com/getlago/lago) —
an open-source, usage-based **billing / metering** platform. Rago is a
Lago-compatible billing service: metering, subscriptions, usage-based pricing,
invoicing (with exact money), payments, webhooks, a GraphQL dashboard API, a
REST API (v1 + v2), documents (PDF / XML), integrations (tax / accounting /
CRM), and a durable background worker — all in one Cargo workspace.

Money is exact (`rust_decimal`, integer cents) end-to-end; there is **no `f64`
money** anywhere in the pipeline.

## Run it

### Option A — Docker Compose (one command)

```bash
cp .env.example .env
docker compose up --build
# API + GraphQL + worker daemon on http://localhost:3000
# add the ClickHouse event store:  docker compose --profile clickhouse up --build
```

### Option B — the single supervising binary

```bash
# Postgres must be reachable via DATABASE_URL.
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/rago
cargo run --release --bin rago-server
```

`rago-server` boots the REST + GraphQL API **and** the scheduler/worker daemon
(subscription crons, dunning, the durable job queue, the daily billing cycle,
and webhook re-delivery) in a single process. Embedded migrations run
automatically on startup.

### Option C — separate processes

```bash
cargo run --release --bin rago-api        # API only
cargo run --release --bin rago-worker      # durable worker / CLI (work, crons, dunning, daemon, ...)
cargo run --release --bin rago-scheduler   # billing scheduler
```

## Live transports (owner-gated)

Every external call — payments (Stripe / GoCardless / Adyen), outbound
webhooks, tax/accounting/CRM aggregators, integration OAuth, and email — goes
through an **injectable transport**. The default build uses in-memory /
capture transports (verified against real provider request/response shapes);
the real `reqwest` HTTP transport and SMTP email transport are compiled in with:

```bash
cargo build --release --features live --bin rago-server
```

With `--features live`, drop the provider keys / SMTP credentials into the
environment (see `.env.example`) and Rago talks to the real services — **no code
change**.

## Configuration

All configuration is via environment variables — see [`.env.example`](.env.example).
Key ones: `DATABASE_URL`, `RAGO_API_ADDR`, `RAGO_TICK_SECS`, `RAGO_BILLING_HOUR`,
`RAGO_RATE_LIMIT`, `RUST_LOG`, and the `RAGO_*` / SMTP live-transport keys.

## API surface

| Area | Endpoint(s) |
|------|-------------|
| Health / metrics | `GET /health`, `GET /metrics` (Prometheus) |
| GraphQL dashboard | `GET|POST /graphql` (queries, mutations, Relay connections) |
| REST v1 | `/api/v1/*` — customers, metrics, plans, subscriptions, events, invoices, wallets, coupons, credit notes, add-ons, quotes, data exports, entitlements, usage alerts, dunning campaigns, lifetime usage, integrations |
| REST v2 | `/api/v2/*` — batch events ingestion, fee estimate, data-API analytics |
| Provider webhooks | `POST /webhooks/{stripe,gocardless,adyen}/:organization_id` |
| Documents | `GET /api/v1/invoices/:id/download(.xml)`, `.../receipt`, `/credit_notes/:id/download` |

Authentication is API-key bearer (`Authorization: Bearer <key>`) on REST; the
GraphQL dashboard resolves the same principal.

## Observability

* **Prometheus** metrics at `GET /metrics`: HTTP request counts, in-flight
  gauge, latency histogram, and domain counters (`rago_invoices_generated_total`,
  `rago_emails_sent_total`, `rago_usage_alerts_fired_total`,
  `rago_webhooks_enqueued_total`, response-class counters, `rago_rate_limited_total`).
* **tracing** spans on every request (and the worker daemon).
* Fixed-window **rate limiting** middleware (`RAGO_RATE_LIMIT`, default 6000/min).

## Workspace layout

```
crates/
  rago-core          models, enums, exact-money primitives
  rago-billing       aggregation, charge models, fees/taxes, invoices, subscriptions, wallets, credit notes
  rago-db            sqlx (Postgres) repositories + embedded migrations
  rago-api           axum REST v1/v2 + GraphQL mount + observability (bins: rago-api, rago-server)
  rago-graphql       async-graphql dashboard (queries, mutations, Relay)
  rago-jobs          durable job queue, scheduler, crons, worker (bins: rago-worker, rago-scheduler)
  rago-integrations  payment providers, webhooks, tax/accounting/CRM, OAuth — injectable transport
  rago-mail          transactional email builders + injectable transport (capture / SMTP)
  rago-documents     dependency-free PDF writer + invoice/credit-note/receipt + invoice XML
  rago-clickhouse    ClickHouse event store (parity with Postgres)
```

## Build & test

```bash
cargo build --workspace --release       # whole product
cargo build --features live --bin rago-server   # live transports
cargo test --workspace                   # unit + integration tests
cargo clippy --workspace                 # lints
```

## Status

Rago is a **functionally-complete, faithful Lago-compatible billing service**:
metering → subscriptions → usage pricing → invoicing (exact money) → payments →
webhooks → documents → GraphQL/REST → analytics → integrations → durable jobs,
with observability and a one-command run story. Remaining work is owner-gated
live-credential wiring, operational hardening, and niche e-invoicing formats.
