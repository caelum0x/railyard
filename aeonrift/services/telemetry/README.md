# services/telemetry — Telemetry

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

High-volume event sink for gameplay, client, and match-lifecycle events. Validates against a typed event schema, buffers, and fans out to the warehouse + realtime liveops dashboards. Feeds balance-sim and anti-cheat signal extraction; carries no PII beyond a hashed account id.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/v1/events` | Ingest a batch of typed telemetry events. |
| `GET` | `/v1/schemas/{event}` | Fetch the registered schema for an event type. |
| `POST` | `/v1/schemas/{event}` | Register/evolve an event schema (additive only). |
| `GET` | `/v1/streams/{topic}/health` | Ingest lag + drop-rate for a topic. |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- gameservers (match events)
- anti-cheat (signals)
- warehouse sink

## Data stores

Kafka/Redpanda (event bus), ClickHouse (warehouse), schema registry.

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
