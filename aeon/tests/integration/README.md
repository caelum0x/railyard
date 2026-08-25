# tests/integration

**SCAFFOLD.** Cross-service / cross-crate integration tests. Not a compiled crate; not a
Cargo workspace member. See ../../MIGRATION.md.

## What this guards

The seams between components — where unit tests in each crate stop and behavior at the
boundary begins:

- **sim ↔ server ↔ client** — a match runs end-to-end in-process (the migrated `no_desync`
  path), rosters load, ticks advance, results finalize.
- **content ↔ engine** — the DSL loader (`engine/ability-dsl`) reads `content/champions/*.ron`,
  interprets abilities, and the duel harness resolves them.
- **service ↔ service** (as services graduate) — e.g. `economy` grant → `inventory` fulfilment,
  `matchmaking` lobby → `gameservers` allocation, using the API envelopes documented in
  `services/*/openapi.yaml`.

## Shape

Integration tests spin the minimum real components (no mocks where a real in-process
component will do), exercise a boundary, and assert the observable contract — not internals.

## Failure = red gate

A boundary regression fails CI even when every unit test is green; that is exactly the
class of bug this layer exists to catch.
