# Matchmaking — service design

> Design notes for `services/matchmaking`. Scaffold; the implementation migrates in later.

## Responsibility

Builds fair 5v5 lobbies. Ingests queue tickets, runs the MMR/role-fill solver, forms teams within a latency + skill window, then hands a confirmed lobby to gameservers for allocation. Owns queue dodge penalties and the ready-check handshake.

## Boundaries

This service owns exactly one concern and exposes it behind the HTTP surface in [openapi.yaml](./openapi.yaml). It does **not** reach into another service's database; cross-service reads go through that service's API.

### Upstream / downstream

- accounts-auth (token verify)
- leaderboards (MMR read)
- gameservers (allocate)

## State & storage

Redis (queue state, ready-check TTLs), Postgres (dodge penalties, queue history).

State is derived from an append-only source of truth where possible so that replays, audits, and reconciliation are always reproducible.

## Failure modes

- **Dependency down** — degrade to read-only / cached responses; never fabricate authoritative state.
- **Duplicate writes** — all mutating endpoints are idempotent by a client-supplied key.
- **Backpressure** — shed load at the edge before corrupting the source of truth.

## Observability

Emits structured events to `telemetry` (hashed account id only), a `/healthz` liveness probe, and RED metrics (rate/errors/duration) per endpoint scraped by the k8s stack in `infra/k8s`.

## Open questions

- Region topology: single-writer vs. multi-region active/active for this data.
- Retention window for historical records before archival to cold storage.
