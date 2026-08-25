# Telemetry — service design

> Design notes for `services/telemetry`. Scaffold; the implementation migrates in later.

## Responsibility

High-volume event sink for gameplay, client, and match-lifecycle events. Validates against a typed event schema, buffers, and fans out to the warehouse + realtime liveops dashboards. Feeds balance-sim and anti-cheat signal extraction; carries no PII beyond a hashed account id.

## Boundaries

This service owns exactly one concern and exposes it behind the HTTP surface in [openapi.yaml](./openapi.yaml). It does **not** reach into another service's database; cross-service reads go through that service's API.

### Upstream / downstream

- gameservers (match events)
- anti-cheat (signals)
- warehouse sink

## State & storage

Kafka/Redpanda (event bus), ClickHouse (warehouse), schema registry.

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
