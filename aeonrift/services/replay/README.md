# services/replay — Replay

**SCAFFOLD.** Not a compiled crate; not a Cargo workspace member.
This directory is a service design surface filled during reconciliation (see ../../MIGRATION.md).

## Role

Captures the deterministic input stream + seed for every match and re-serves it for spectating, post-game review, and anti-cheat evidence. Because the sim is deterministic, a replay is just (seed, roster, input log) — a few KB — replayed through sim-core, not a video.

## Planned endpoints

| Method | Path | Purpose |
|--------|------|---------|
| `POST` | `/v1/replays` | Finalize + store a match input log (from gameservers). |
| `GET` | `/v1/replays/{match_id}` | Fetch replay metadata + input-log location. |
| `GET` | `/v1/replays/{match_id}/inputs` | Stream the raw input log for playback. |
| `GET` | `/v1/replays/search` | Search replays by account/champion/patch. |

All endpoints require a valid access token from `accounts-auth` unless noted; responses use the standard envelope (`{ ok, data, error, meta }`).

## Depends on

- gameservers (finalize)
- anti-cheat (evidence)
- engine/sim-core (playback)

## Data stores

Object store (input logs), Postgres (replay index).

## Laws inherited

- **Server-authoritative** — the client is never trusted for state this service owns.
- **Immutable records** — ledgers/audit logs append; they are not edited in place.
- **No binaries in git** — large payloads live in the content-addressed store / CDN.

See also: [design.md](./design.md), [openapi.yaml](./openapi.yaml).
