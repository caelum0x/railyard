# Anti-Cheat — service design

> Design notes for `services/anti-cheat`. Scaffold; the implementation migrates in later.

## Responsibility

Server-authoritative integrity: consumes telemetry + sim divergence signals, scores sessions for scripting/botting/input anomalies, and issues enforcement actions back to accounts-auth. Because the sim is deterministic and server-authoritative, most detection is divergence-based, not client-trust-based.

## Boundaries

This service owns exactly one concern and exposes it behind the HTTP surface in [openapi.yaml](./openapi.yaml). It does **not** reach into another service's database; cross-service reads go through that service's API.

### Upstream / downstream

- telemetry (signals)
- accounts-auth (enforcement)
- replay (evidence)

## State & storage

ClickHouse (signal store), Postgres (enforcement records).

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
