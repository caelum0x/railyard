# services/economy — Economy

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

Authoritative ledger for hard + soft currency, purchases, refunds, and battle-pass credit. MIGRATE: Railyard/Mint + CalDesk economy wiring lands here (see ../../MIGRATION.md). Every grant/spend is a double-entry transaction; balances are derived, never mutated in place.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/v1/wallets/{account_id}` | Fetch currency balances for an account. |
| `POST` | `/v1/wallets/{account_id}/grants` | Grant currency (purchase, reward, refund). |
| `POST` | `/v1/wallets/{account_id}/spends` | Spend currency against a store order. |
| `POST` | `/v1/orders` | Create a store order (idempotent by client key). |
| `POST` | `/v1/orders/{id}/refund` | Refund a settled order. |
| `GET` | `/v1/ledger/{account_id}` | Paginated transaction history. |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- accounts-auth (token verify)
- inventory (fulfilment)
- store web surface

## Data stores

Postgres (double-entry ledger, orders), Redis (idempotency keys).

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
