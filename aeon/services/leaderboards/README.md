# services/leaderboards — Leaderboards & Ranked

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

Owns ranked ladders, MMR/rank tiers, and seasonal resets. Consumes match results, updates rating (Glicko-2 style), and serves ladder pages to web + client. Matchmaking reads MMR from here; it does not write ranks itself.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/v1/results` | Ingest a finished match result (updates MMR). |
| `GET` | `/v1/ladders/{season}/{queue}` | Paginated ranked ladder. |
| `GET` | `/v1/ranks/{account_id}` | Current rank/tier/LP + MMR. |
| `POST` | `/v1/seasons/{id}/reset` | Apply a seasonal soft reset. |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- gameservers (results)
- matchmaking (MMR read)

## Data stores

Postgres (ratings, seasons), Redis (sorted-set ladders).

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
