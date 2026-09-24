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

`rago-server` boots the REST + GraphQL API, the operator console at `/admin`,
**and** the scheduler/worker daemon in a single process: subscription crons,
the clock registry (invoice finalization, wallet top-ups/expiry, coupon expiry,
usage snapshots), dunning, the once-a-day billing cycle (bill → invoice →
webhooks → payment collection through each organization's provider), the
durable job queue, and outbound webhook delivery. Embedded migrations run
automatically on startup.

### Option C — separate processes

```bash
cargo run --release --bin rago-api        # API only
cargo run --release --bin rago-worker      # durable worker / CLI (work, crons, dunning, daemon, ...)
cargo run --release --bin rago-scheduler   # billing scheduler
```

## Live transports

Every external call — payments (Stripe / GoCardless / Adyen / Creem / Dodo
Payments / Iyzico), Mint holds, outbound
webhooks, tax/accounting/CRM aggregators, integration OAuth, SSO, the finance
assistant, and email — goes through an **injectable transport**. Production
builds use the real `reqwest` HTTP and SMTP transports:

```bash
cargo build --release --features live --bin rago-server   # what the Dockerfile builds
```

A build without `live` uses a *disabled* transport: outbound calls fail and are
recorded as failures (nothing is ever marked paid or delivered without a real
call). Tests inject scripted in-memory transports.

Payment providers are configured per organization from the console (Settings →
Connect Stripe, GoCardless, Adyen, Creem, Dodo Payments or Iyzico). Secrets are
sealed with `RAGO_ENCRYPTION_KEY`. Single-tenant deployments can instead set
`RAGO_PAYMENT_PROVIDER` and that provider's `RAGO_*` keys (see `.env.example`).
Stripe, GoCardless and Adyen charge a stored payment method: each customer gets
a Stripe customer on first charge, and charges use their saved payment method.
Creem and Dodo Payments (merchants of record) and Iyzico collect through a
hosted checkout. The invoice's payment waits for the customer, its pay link is
sent in `payment.requires_action` and shown in the customer portal, and the
signed provider webhook settles it. Creem's API can only refund whole payments;
Dodo and Iyzico refund any amount.

### Customer portal

`POST /api/v1/customers/:external_id/portal_sessions` issues a signed, expiring
link to `/portal/:token` (24 hours by default, 30 days at most;
`DELETE` on the same path revokes every open link). The portal is free and works
with any provider or none. It shows invoices with PDF download and a Pay
button, usage this period per billable metric, and wallet balances with top-up.
Customers can cancel subscriptions there, and can update their payment method
when Stripe is the provider. `GET /customers/:external_id/portal_url` returns
a portal link whenever the organization does not collect with Stripe.

### Real-time wallets (Mint holds)

`PUT /api/v1/wallets/:id/realtime` with `{ "wallet": { "realtime_enabled": true,
"realtime_policy": "reject" | "flag", "mint_account_id": "<customer's Mint
account>" } }` turns a USD wallet into a real-time wallet. Every event posted
to `/events` is priced at its marginal cost by the plan's charge models and
held on the customer's Mint account. The hold's idempotency key comes from the
event's `transaction_id`, so a retried event is only held once. If the balance
does not cover the event, `reject` answers 422 `wallet_balance_insufficient`
and does not store it, while `flag` stores it and reports `event.error`. If Mint
is unreachable, `reject` answers 503 and fails closed. When the invoice is
finalized, the holds are captured to `RAGO_MINT_PAYEE_ACCOUNT` up to the amount
due and applied as prepaid credit, and any surplus is released. Voiding an
invoice releases holds that are still reserved for it. Set `RAGO_MINT_URL`,
`RAGO_MINT_API_KEY` and `RAGO_MINT_PAYEE_ACCOUNT` to enable real-time wallets.

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
| Provider webhooks | `POST /webhooks/{stripe,gocardless,adyen,creem,dodo,iyzico}/:organization_id` |
| Customer portal | `GET /portal/:token` (+ invoice PDF, pay, wallet top-up, cancel, payment method) |
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
cargo test --workspace                   # DB-less tests (pg suites skipped)
cargo clippy --workspace                 # lints
```

## Running the tests

### Fast gate — no database needed

```bash
cargo test --workspace          # unit + in-memory integration tests
cargo test --workspace --lib    # unit tests only (fastest)
```

The Postgres-backed suites are gated behind the opt-in `pg-tests` cargo
feature, so a plain `cargo test` skips them cleanly instead of panicking on a
missing `DATABASE_URL`.

### Full gate — real Postgres (`#[sqlx::test]`)

The money-correctness suites (idempotency guards, re-bill guard, charge-retry
dedupe, aggregator parity, GraphQL admin, REST CRUD) run against a real,
per-test isolated + migrated Postgres:

```bash
colima start                                    # macOS: bring up Docker first
docker compose up -d postgres                   # postgres:17 on :5432
export DATABASE_URL=postgres://postgres:postgres@localhost:5432/rago
cargo test --workspace --features pg-tests      # everything, incl. pg suites
```

Notes:

* If another Postgres already owns `:5432`, pick a free host port:
  `RAGO_PG_PORT=5439 docker compose up -d postgres` and point `DATABASE_URL`
  at `localhost:5439`.
* `#[sqlx::test]` creates a fresh migrated database per test, so the connected
  user needs `CREATEDB` rights — the compose `postgres` superuser is fine.
* Per-crate runs also work: `cargo test -p rago-jobs -F pg-tests`.

## Status

Rago is a **functionally-complete, faithful Lago-compatible billing service**:
metering → subscriptions → usage pricing → invoicing (exact money) → payments →
webhooks → documents → GraphQL/REST → analytics → integrations → durable jobs,
with observability and a one-command run story. Remaining work is owner-gated
live-credential wiring, operational hardening, and niche e-invoicing formats.
