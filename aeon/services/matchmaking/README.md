# services/matchmaking — Matchmaking

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

Builds fair 5v5 lobbies. Ingests queue tickets, runs the MMR/role-fill solver, forms teams within a latency + skill window, then hands a confirmed lobby to gameservers for allocation. Owns queue dodge penalties and the ready-check handshake.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/v1/tickets` | Enqueue a matchmaking ticket (party, roles, region). |
| `DELETE` | `/v1/tickets/{id}` | Cancel a queued ticket. |
| `GET` | `/v1/tickets/{id}` | Poll ticket state (queued/found/ready-check). |
| `POST` | `/v1/matches/{id}/ready` | Accept the ready-check for a proposed match. |
| `GET` | `/v1/queues/{region}/stats` | Current queue depth + estimated wait per role. |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- accounts-auth (token verify)
- leaderboards (MMR read)
- gameservers (allocate)

## Data stores

Redis (queue state, ready-check TTLs), Postgres (dodge penalties, queue history).

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
