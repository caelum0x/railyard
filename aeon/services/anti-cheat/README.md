# services/anti-cheat — Anti-Cheat

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

Server-authoritative integrity: consumes telemetry + sim divergence signals, scores sessions for scripting/botting/input anomalies, and issues enforcement actions back to accounts-auth. Because the sim is deterministic and server-authoritative, most detection is divergence-based, not client-trust-based.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/v1/signals` | Ingest an integrity signal (divergence, input anomaly). |
| `GET` | `/v1/sessions/{id}/score` | Current risk score + contributing signals. |
| `POST` | `/v1/enforcements` | Issue an enforcement (warn/suspend/ban) to accounts-auth. |
| `GET` | `/v1/enforcements/{account_id}` | Enforcement history for an account. |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- telemetry (signals)
- accounts-auth (enforcement)
- replay (evidence)

## Data stores

ClickHouse (signal store), Postgres (enforcement records).

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
