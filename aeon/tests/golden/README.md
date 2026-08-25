# tests/golden

**SCAFFOLD.** Golden replay tests. Not a compiled crate; not a Cargo workspace member.
See ../../MIGRATION.md.

## What this guards

Long-horizon behavioral stability. A golden is a full recorded match — `(seed, roster,
input log)` plus the expected outcome (winner, final world hash, key event log). Replaying
it through `engine/sim-core` must reproduce the recorded outcome bit-for-bit. Goldens catch
*silent behavioral drift* that a short determinism scenario would miss.

## Descriptor

Each golden (see [sample-golden.ron](./sample-golden.ron)) points at:
- a compact input log (stored, not video — the sim is deterministic),
- the expected final world hash + winner,
- an expected **event digest** (ordered key events: kills, objectives, ability resolutions).

## Refresh policy

Goldens are **intentionally rigid**. When a balance/logic change legitimately alters an
outcome, the golden is regenerated from a reviewed run **in the same change** and the diff
is inspected in review — never auto-accepted. An unexplained golden diff is a red gate.
