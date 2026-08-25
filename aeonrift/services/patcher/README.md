# services/patcher — Patcher

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

Serves the client update manifest and content-addressed chunk map. Clients diff their local manifest against the channel head (live/pbe) and pull only changed blake3 chunks from the CDN. Mirrors the no-binaries-in-git law: the patcher indexes hashes, the CDN serves the bytes.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/v1/channels/{channel}/manifest` | Current manifest (version + chunk hashes). |
| `GET` | `/v1/channels/{channel}/manifest/{version}` | Pinned historical manifest. |
| `POST` | `/v1/channels/{channel}/publish` | Publish a new manifest to a channel. |
| `GET` | `/v1/chunks/{blake3}/location` | Resolve a chunk hash to its CDN URL(s). |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- cdn (chunk delivery)
- assets-manifests (hash source)

## Data stores

Object store (manifests), Postgres (channel head + publish log).

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
