# tests/netcode

**SCAFFOLD.** Cross-crate netcode tests. Not a compiled crate; not a Cargo workspace member.
See ../../MIGRATION.md.

## What this guards

The client-side prediction + server reconciliation spine in `games/aeonrift-moba`
(and later `engine/net` / `engine/protocol`). The invariant: a predicting client, after
reconciling against authoritative server snapshots, converges to **exactly** the server's
world — **no desync** — under packet loss, reorder, and jitter.

## Scenarios

Each scenario (see [sample-scenario.ron](./sample-scenario.ron)) describes a network profile
(latency, loss, jitter) plus an input stream, and asserts:

1. **No desync** — client world hash == server world hash after reconciliation each tick.
2. **Bounded rollback** — mispredictions re-simulate within the rollback window.
3. **Input integrity** — every client input is applied exactly once, server-authoritatively.

Mirrors the migrated `no_desync` integration test, lifted to a cross-crate, profile-driven gate.

## Failure = red gate

A desync means prediction and authority diverged; that is a correctness bug in reconciliation
or in determinism — fix the code, do not loosen the tolerance.
