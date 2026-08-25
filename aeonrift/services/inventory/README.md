# services/inventory — Inventory

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

Source of truth for what a player owns: champions, skins, wards, emotes, battle-pass unlocks. Fulfils grants from economy, exposes the durable owned-set to the client and champion-pages, and enforces entitlement checks at match start.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/v1/inventories/{account_id}` | List everything an account owns. |
| `POST` | `/v1/inventories/{account_id}/grants` | Grant an item (from economy fulfilment). |
| `GET` | `/v1/inventories/{account_id}/entitlements/{sku}` | Check a single entitlement. |
| `POST` | `/v1/inventories/{account_id}/loadouts` | Save a champion loadout/skin selection. |
| `GET` | `/v1/catalog` | Catalog of grantable SKUs (mirror of store catalog). |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- economy (fulfilment)
- accounts-auth (token verify)
- content/champions (SKU map)

## Data stores

Postgres (owned items, loadouts), read cache in Redis.

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
