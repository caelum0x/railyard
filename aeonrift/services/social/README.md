# services/social — Social

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

Friends, parties, presence, and text chat with moderation hooks. Parties feed matchmaking as a single ticket; presence drives the friends list; chat is filtered and reportable, with reports flowing to anti-cheat/trust review.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `GET` | `/v1/friends/{account_id}` | List friends + presence. |
| `POST` | `/v1/friends/{account_id}/requests` | Send a friend request. |
| `POST` | `/v1/parties` | Create a party. |
| `POST` | `/v1/parties/{id}/members` | Invite/join a party member. |
| `POST` | `/v1/parties/{id}/messages` | Post a moderated party chat message. |
| `POST` | `/v1/reports` | Report a player (behavior/chat). |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- accounts-auth (token verify)
- matchmaking (party ticket)
- anti-cheat (reports)

## Data stores

Redis (presence, party state), Postgres (friends, reports).

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
