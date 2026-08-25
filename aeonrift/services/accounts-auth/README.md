# services/accounts-auth — Accounts & Auth

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

Owns player identity: registration, login, session tokens (JWT access + refresh), OAuth/console linking, ban/suspension state, and age-gate/consent records feeding the compliance surface (COPPA/GDPR). Every other service trusts a signed access token minted here; nothing else issues credentials.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/v1/accounts` | Register a new account (email + age gate). |
| `POST` | `/v1/sessions` | Log in, returns access + refresh token pair. |
| `POST` | `/v1/sessions/refresh` | Exchange a refresh token for a fresh access token. |
| `DELETE` | `/v1/sessions/current` | Log out / revoke the current refresh token. |
| `POST` | `/v1/accounts/{id}/links` | Link a console / Steam / OAuth identity. |
| `GET` | `/v1/accounts/{id}` | Fetch public account profile + ban state. |
| `POST` | `/v1/accounts/{id}/consent` | Record a consent / data-processing decision. |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- (none — root of trust)

## Data stores

Postgres (accounts, sessions, consents), Redis (refresh-token revocation list).

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
