# Ledgerline

## Deployed (2026-10-28)

Live on railyard-box (`130.61.113.116`), `profile: ledgerline` in
`infra-box/stacks/railyard/docker-compose.yml` (new profile this pass — unlike
Vend/Toolgate, this app had no Dockerfile and no prepared compose/Caddy
scaffolding at all; added a standalone distroless `Dockerfile`, matching the
Toolgate/Vend build shape, since this is a single-crate package with no path
deps outside this directory). Container port 8093 -> host `127.0.0.1:4300` ->
Caddy `ledgerline.{$RAILYARD_DOMAIN}`. Own Postgres database `ledgerline`
(role `ledgerline_app`). `sqlx::migrate!()` runs at startup, confirmed by a
real `GET /readyz` -> `204` on first boot.

Wired to the real, already-live Railyard Mint (`MINT_BASE=http://mint:8080`,
shadow mode) with four real, freshly created Mint accounts matching the
account policy: revenue (`acct_501M605A07MFATN2Z77NB90VE04`), COGS
(`acct_501M605A0EFQ094TH184A7VQC2H`), one provider
(`acct_301M605A0N8TTDASTQJ742S7DSW`), and one customer
(`acct_101M605A0VXZBVWHEV71G82EMDJ`, `customer_id: "deploy-verify"`) in
`LEDGERLINE_ACCOUNT_POLICY_JSON`.

Verified live end-to-end against the real deployed container: a real
success-path `POST /v1/events` correctly reached the real Mint and was
refused with a real `402 insufficient_funds` (the customer account has a
real zero balance — cross-checked directly against Mint's own
`GET /v1/accounts/:id/balance`, identical `0` both ways, so this is a real
round trip, not a stub — same funding wall every other Railyard sub-app
hits); a real failed-outcome event (zero amounts) correctly settled with
`201` and no Mint calls; `GET /v1/margins`, `GET /v1/margins/:request_id`
and `GET /v1/margins/summary?group_by=customer` all return the real stored
row/rollup; an event naming a non-allowlisted account was correctly refused
`403` by the account-policy guard *before* any Mint call; `/admin` renders
(200).

One step remains, infra not code, same single blocker as Toolgate and Vend:
the public DNS A record (`ledgerline.ereboslab.com` -> `130.61.113.116`,
proxied) hasn't been created (the only Cloudflare credential available in
this session is a read-only MCP connection). Caddy is already configured
and reloaded; routing confirmed correct now (`Host: ledgerline.ereboslab.com`
-> real `308` redirect).

**Status: runnable Sx0 product slice.** Ledgerline accepts AI usage events,
settles their customer revenue and provider COGS through Mint, and exposes the
resulting request-level gross margin. It is a side-car margin lens: it does not
replace Mint's money ledger or Relay's inference gateway.

The complete product thesis and phased plan live in
[`docs/apps-vision/ledgerline.md`](../../docs/apps-vision/ledgerline.md).

## Shipped surface

| Route | Auth | Purpose |
|---|---|---|
| `GET /healthz` | public | Process liveness |
| `GET /readyz` | public | Postgres readiness |
| `POST /v1/events` | `X-API-Key` | Validate and idempotently settle one billable request |
| `GET /v1/margins/:request_id` | `X-API-Key` | Fetch one request's revenue, COGS, margin, dimensions, and Mint references |
| `GET /v1/margins` | `X-API-Key` | Explore margins; optional `customer_id`, `feature`, `model`, and `limit` filters |

A successful event makes three real, retry-safe Mint calls with deterministic
operation keys:

1. hold the customer-facing revenue amount;
2. capture that hold to the revenue account;
3. transfer provider COGS from the configured COGS account to the provider.

These three steps are driven as one **atomic-or-compensated** settlement by the
`SettlementCoordinator` over a durable outbox (`src/settle.rs`, `src/outbox.rs`).
Each step is recorded in a per-request saga (`pending → committed`) *before and
after* the Mint call, so a crash mid-sequence is recoverable:

- a resumed attempt re-runs the trio, skipping already-committed steps (Mint's
  per-step idempotency keys make any redundant call a no-op); and
- if a step ultimately fails, the coordinator reverses the already-committed
  steps — release the hold, move captured revenue back, undo the COGS transfer —
  leaving the request **fully settled or fully compensated, never half-settled**
  and never double-charged. A retry after a compensated attempt gets fresh
  per-attempt keys and settles cleanly exactly once.

All three operations share the event's `request_id` as their Mint reference.
Failed requests are accepted only when both amounts are zero and create no Mint
movements, preserving ADR-0002. Replaying an identical `idempotency_key` returns
the original record with `idempotent_replay: true`; changing the event behind a
key returns `409`.

Settlement (money) and the request-level margin projection are two writes. A
crash after a request settles on Mint but before its margin row lands would leave
the money correct yet the request's gross margin invisible. The recovery worker
(`src/recovery.rs`) closes that window: alongside driving stuck sagas, it runs a
projection reconciliation sweep that finds any `Settled` saga whose `margin_events`
row is missing (a Postgres anti-join in production) and rebuilds it from the
saga's authorized event binding. The backfill touches only the read model — never
Mint — and `save` is idempotent by `idempotency_key`, so a racing client retry can
never double-project or double-charge.

## Run locally

Ledgerline is a standalone Cargo workspace, deliberately outside the root
desktop workspace. Start a Postgres database and a configured `mint-server`, then:

```bash
cd apps/ledgerline
DATABASE_URL='postgres://postgres:postgres@localhost:5432/ledgerline' \
MINT_BASE='http://127.0.0.1:8088' \
MINT_API_KEY='mint-internal-key' \
LEDGERLINE_API_KEY='ledgerline-client-key' \
cargo run --bin ledgerline-server
```

The server runs its embedded Postgres migration at startup and listens on
`0.0.0.0:8093` by default.

### Environment contract

| Variable | Required | Meaning |
|---|---:|---|
| `DATABASE_URL` | yes | Postgres DSN for event metadata and margin projections |
| `MINT_BASE` | yes | Base URL of the live Mint service |
| `MINT_API_KEY` | yes | Mint's internal `X-API-Key` credential |
| `LEDGERLINE_API_KEY` | yes | Credential clients send to Ledgerline as `X-API-Key` |
| `LEDGERLINE_ACCOUNT_POLICY_JSON` | yes | Allowlisted `revenue_accounts` / `cogs_accounts` / `provider_accounts` plus a `customers` map (`customer_id` to Mint account); events naming other accounts get `403 forbidden` before any Mint call |
| `LEDGERLINE_ALLOW_UNRESTRICTED_ACCOUNTS` | no | `1` skips the policy (local development only) |
| `LEDGERLINE_BIND_ADDR` | no | Listener address; default `0.0.0.0:8093` |
| `RUST_LOG` | no | Tracing filter; default `info` |

No deployment silently starts without its database, credentials or account policy.

```bash
export LEDGERLINE_ACCOUNT_POLICY_JSON='{
  "revenue_accounts": ["acct_revenue"],
  "cogs_accounts": ["acct_cogs"],
  "provider_accounts": ["acct_provider_openai"],
  "customers": {"customer-1": "acct_customer_1"},
  "allow_unmapped_customers": false
}'
```

## Margin lens

- `GET /v1/margins?customer_id=&feature=&model=&from=&to=&before=&limit=` — request-level
  records newest first; page with `before=<created_at of the last row>`.
- `GET /v1/margins/summary?group_by=customer|feature|model|day&from=&to=` — rollups
  with revenue, COGS, margin, margin bps, failed and negative-margin counts, totals
  and the list of buckets losing money.
- `GET /admin` — the margin console (rollups, negative-margin alerts, recent
  requests); it calls the API above with your `X-API-Key`.

## Ingest an event

```bash
curl --fail-with-body http://127.0.0.1:8093/v1/events \
  -H 'content-type: application/json' \
  -H 'x-api-key: ledgerline-client-key' \
  --data '{
    "idempotency_key": "rf-job-01JABC",
    "request_id": "job_01JABC",
    "customer_id": "reelforge-internal",
    "feature": "image-to-video",
    "model": "wan-2.2-i2v",
    "source": "reelforge",
    "outcome": "succeeded",
    "revenue_micros": 2400000,
    "cogs_micros": 1750000,
    "customer_account": "acct_customer",
    "revenue_account": "acct_reelforge_revenue",
    "cogs_account": "acct_reelforge_cogs",
    "provider_account": "acct_runpod"
  }'
```

The returned `margin_micros` is exact integer math (`650000` above), and
`margin_bps` is `2708` (27.08%). Negative margins are valid and visible.

## Validate

```bash
cd apps/ledgerline
cargo fmt --all -- --check
cargo check --all-targets
cargo test
cargo clippy --all-targets --all-features -- -D warnings
```

The saga's money invariants and the projection backfill are proven end to end
against a real database by the integration suite in `tests/pg_settlement.rs`. It
is gated on `DATABASE_URL`, so plain `cargo test` stays green with no database;
point it at Postgres to exercise the real outbox and store:

```bash
DATABASE_URL='postgres://user@localhost:5432/ledgerline' \
  cargo test --test pg_settlement
```

## Current Sx0 boundary

This slice deliberately stops before Sx1 rating catalogs and Relay cost-feed
subscription. Clients currently provide the already-rated revenue and exact or
estimated COGS amounts. Postgres stores operational metadata and projections;
Mint remains the authoritative ledger.

The settlement outbox mechanism (atomic-or-compensated saga) is backed by the
`settlement_outbox` Postgres table in production. Every Mint step transition is
persisted, so retrying the same request after a restart resumes or compensates
from its durable state; `MemoryOutbox` remains only for focused tests. Automatic
replay without an inbound retry is covered by the recovery worker, which both
finishes stuck sagas and backfills settled-but-unprojected margin rows on a fixed
cadence. The one-million-event scale test remains an Sx0 operational follow-up.
