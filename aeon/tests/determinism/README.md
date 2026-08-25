# tests/determinism

**SCAFFOLD.** Cross-crate determinism tests. Not a compiled crate; not a Cargo workspace
member (per-crate unit tests live in each crate). See ../../MIGRATION.md.

## What this guards

The core law: given the same `(seed, roster, input log)`, `engine/sim-core` must produce
the **same world hash on every tick, on every platform**. Determinism is what makes replays,
netcode reconciliation, and divergence-based anti-cheat possible — so it gets its own gate.

## Scenarios

Each scenario in this directory (see [sample-scenario.ron](./sample-scenario.ron)) is a fixed
input stream + seed with an expected terminal world hash. The gate:

1. Loads the scenario.
2. Runs it through `sim-core` for the recorded tick count.
3. Asserts the final (and periodic) world hash equals the recorded golden hash.
4. Runs it a second time and asserts run-to-run equality (no wall-clock / HashMap-iteration leaks).

## Failure = red gate

Any hash mismatch fails CI. The usual culprits: unordered iteration, floating-point
non-determinism, wall-clock reads, or unseeded RNG. Fix the source of non-determinism —
never "rebaseline" a scenario to make it pass.
