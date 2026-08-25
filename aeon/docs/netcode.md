# Netcode

**SCAFFOLD.** How aeonrift stays in sync across the network. Builds on the determinism
guarantee from [architecture.md](./architecture.md).

## The premise

Because `engine/sim-core` is deterministic — same `(seed, roster, input log)` → same world
hash every tick — the network only needs to agree on **inputs**, not state. This is the whole
game: ship inputs, replay deterministically, reconcile on divergence.

## Server-authoritative predict / reconcile

Today's spine lives in `games/aeonrift-moba` (server + client modules on `sim-core`); it
graduates to real UDP transport in `engine/net` + `engine/protocol` (bevy_replicon / lightyear).

```
client: apply local input immediately (predict) ──▶ send input to server
server: authoritative sim advances a tick ──▶ broadcasts snapshot (tick, hash)
client: on snapshot, if predicted != authoritative → roll back to snapshot tick,
        re-apply buffered inputs (reconcile), within a bounded rollback window
```

## Invariants (gated by `tests/netcode/`)

1. **No desync** — after reconciliation, client world hash == server world hash every tick.
2. **Bounded rollback** — re-simulation never exceeds the rollback window.
3. **Inputs applied once** — every input is applied exactly once, server-authoritatively.

The migrated `no_desync` integration test is the seed of this gate.

## Why it matters downstream

- **Replays** (`services/replay`) are just the input log replayed through the same sim.
- **Anti-cheat** (`services/anti-cheat`) flags clients whose reported state diverges from the
  authoritative sim — cheating is detectable *because* the sim is deterministic.
- **Spectating** is a delayed replay of the authoritative input stream.

## Transport plan

| Stage | Where | Transport |
|-------|-------|-----------|
| Now | `games/aeonrift-moba` | in-process predict/reconcile spine |
| Next | `engine/net` + `engine/protocol` | UDP (bevy_replicon / lightyear) |

See `tests/netcode/sample-scenario.ron` for the profile-driven gate shape.
