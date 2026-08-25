# LiveOps — service design

> Design notes for `services/liveops`. Scaffold; the implementation migrates in later.

## Responsibility

Runtime configuration + campaign control plane: feature flags, event windows (double-XP, rotations), remote balance overrides, and the ops dashboard. Changes are versioned and audited; a bad flag is rolled back, never edited in place.

## Boundaries

This service owns exactly one concern and exposes it behind the HTTP surface in [openapi.yaml](./openapi.yaml). It does **not** reach into another service's database; cross-service reads go through that service's API.

### Upstream / downstream

- accounts-auth (admin authz)
- telemetry (impact metrics)
- all services (config pull)

## State & storage

Postgres (versioned config, audit log), Redis (fast config read).

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
