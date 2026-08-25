# services/liveops — LiveOps

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

Runtime configuration + campaign control plane: feature flags, event windows (double-XP, rotations), remote balance overrides, and the ops dashboard. Changes are versioned and audited; a bad flag is rolled back, never edited in place.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/v1/flags` | Fetch the active flag/config bundle for a client build. |
| `PUT` | `/v1/flags/{key}` | Set/override a flag (audited, versioned). |
| `POST` | `/v1/campaigns` | Schedule a liveops event window. |
| `GET` | `/v1/campaigns/active` | Currently active event windows. |
| `POST` | `/v1/rollbacks/{version}` | Roll the config bundle back to a version. |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- accounts-auth (admin authz)
- telemetry (impact metrics)
- all services (config pull)

## Data stores

Postgres (versioned config, audit log), Redis (fast config read).

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
