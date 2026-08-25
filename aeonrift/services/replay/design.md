# Replay — service design

> Design notes for `services/replay`. Scaffold; the implementation migrates in later.

## Responsibility

Captures the deterministic input stream + seed for every match and re-serves it for spectating, post-game review, and anti-cheat evidence. Because the sim is deterministic, a replay is just (seed, roster, input log) — a few KB — replayed through sim-core, not a video.

## Boundaries

This service owns exactly one concern and exposes it behind the HTTP surface in [openapi.yaml](./openapi.yaml). It does **not** reach into another service's database; cross-service reads go through that service's API.

### Upstream / downstream

- gameservers (finalize)
- anti-cheat (evidence)
- engine/sim-core (playback)

## State & storage

Object store (input logs), Postgres (replay index).

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
