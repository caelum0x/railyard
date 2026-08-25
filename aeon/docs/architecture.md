# Architecture

**SCAFFOLD.** System overview for the aeonrift monorepo. Complements the root plans
(`AEONRIFT-MASTER-PLAN.md`, `AEONRIFT-GRAPH.md`) and `MIGRATION.md`.

## The shape

Aeonrift is a **deterministic, server-authoritative** MOBA built as a Rust monorepo. Three
tiers:

```
engine/     shared kernel (sim-core, combat, ability-dsl, net, protocol, ...)
games/      aeonrift-moba (5v5), aeonrift-ascendancy (mobile cards, lib+cdylib)
services/   backend/live-ops (accounts-auth, matchmaking, gameservers, economy, ...)  [scaffold]
web/ infra/ docs/ tests/   supporting surfaces                                        [scaffold]
content/    data-driven champions/items/runes + closed _schema vocabulary
```

## The deterministic kernel

`engine/sim-core` is the heart: a fixed-tick, seeded-RNG world where the same
`(seed, roster, input log)` always yields the same world hash. `engine/combat` is a facade
over it; `engine/ability-dsl` interprets champion specs from `content/champions/*.ron`
against the closed vocabulary in `content/_schema/`.

Determinism is not a nicety — it is the load-bearing property. It makes possible:
- **Netcode** — predict/reconcile with bounded rollback (`games/aeonrift-moba`, later `engine/net`).
- **Replays** — a match is `(seed, roster, inputs)`, a few KB, not a video (`services/replay`).
- **Anti-cheat** — cheating shows up as *divergence* from the authoritative sim (`services/anti-cheat`).

## Services tier (scaffold)

Twelve services, each owning exactly one concern behind an HTTP surface
(`services/*/openapi.yaml`). They never reach into each other's databases; cross-service reads
go through APIs. `accounts-auth` is the root of trust; every other service verifies its tokens.

## Data flow (a match)

```
client → matchmaking (ticket) → gameservers (allocate) → sim-core (authoritative match)
       → telemetry (events) → leaderboards (result) → replay (finalize)
```

## Laws (inherited from the master plan)

1. **Deterministic** simulation.
2. **Closed vocabulary** — content may only use engine-implemented primitives.
3. **Per-champion tests** — every champion has duel gates.
4. **No binaries in git** — blobs live in the content-addressed store / CDN.
5. **Original IP only**.
6. **Server-authoritative** — the client is never trusted for owned state.

See [netcode.md](./netcode.md), [dsl-spec.md](./dsl-spec.md), and [runbooks.md](./runbooks.md).
