# tests/e2e

**SCAFFOLD.** End-to-end tests across the full player journey. Not a compiled crate; not a
Cargo workspace member. See ../../MIGRATION.md.

## What this guards

The critical user flows, front to back, closest to what a real player does:

1. **Onboard** — register (age gate + consent) → log in → receive tokens (`accounts-auth`).
2. **Acquire** — browse `web/store` → order (`economy`) → item appears in `inventory`.
3. **Play** — queue (`matchmaking`) → allocate (`gameservers`) → deterministic match → result.
4. **Progress** — result updates `leaderboards`; replay is finalized in `replay`.
5. **Web** — the matching champion page renders in `web/champion-pages`; patch notes load.

## Shape

E2E runs against a stood-up stack (the local `infra/docker` compose for backing stores, plus
the graduated services) and asserts the *outcome a player would see*, not internal calls.
These are the slowest, highest-signal tests and run on a reduced cadence (pre-release + nightly).

## Failure = release blocker

A broken critical flow blocks release even if unit + integration are green.
