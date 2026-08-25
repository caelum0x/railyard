# services/gameservers — Gameservers (Orchestrator)

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

Allocates and lifecycles authoritative match instances. Given a confirmed lobby it reserves a warm sim-core server in the nearest region, streams the roster + seed, and reaps the instance on match end. Wraps the fleet autoscaler (Agones-style) and exposes health for liveops.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/v1/allocations` | Allocate a match server for a confirmed lobby. |
| `GET` | `/v1/allocations/{id}` | Fetch connection info + server state. |
| `DELETE` | `/v1/allocations/{id}` | Force-release an allocation (match ended/aborted). |
| `GET` | `/v1/fleet/{region}` | Fleet capacity, warm pool size, and utilization. |
| `POST` | `/v1/fleet/{region}/drain` | Cordon + drain a region for a deploy. |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- matchmaking (lobby handoff)
- telemetry (match lifecycle)
- replay (finalize)

## Data stores

In-cluster fleet state (k8s CRDs), Redis (allocation index).

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
