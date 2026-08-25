# aeonrift

The franchise monorepo skeleton — the consolidation target that `moba/` (sim-core,
M2 combat, M5 ability-DSL, roster/cards/asset branches) and the `moon-lol` fork
(`fork/original-ip`: render, netcode, RL bots, real champion plugins) migrate into.

This repo is the **compiling skeleton**, not the full merge: every crate under
`engine/`, `games/`, and `tools/` is a real Cargo crate with a role doc comment and
a `// MIGRATE:` pointer to where the existing implementation lives. The dependency
graph (`combat -> sim-core`, `ability-dsl -> combat`, `net -> sim-core`, ...) is
wired as real Cargo path deps so the workspace graph is queryable from day one.

## Plans
- Tree / file-budget: [AEONRIFT-MASTER-PLAN.md](../AEONRIFT-MASTER-PLAN.md)
- Typed architecture graph: [AEONRIFT-GRAPH.md](../AEONRIFT-GRAPH.md)
- Universe / naming: [AEONRIFT-UNIVERSE.md](../AEONRIFT-UNIVERSE.md)
- Two-repo + asset scale: [MOBA-ARCHITECTURE.md](../MOBA-ARCHITECTURE.md)

## Layout
```
engine/            shared kernel (14 crates): sim-core combat ability-dsl net protocol
                   pathfinding vision economy ai-bots render particle audio-engine ui-kit platform
games/             aeonrift-moba (5v5), aeonrift-ascendancy (mobile cards, lib+cdylib)
tools/             champion-gen card-gen balance-sim content-lint asset-cli localization mcp graph
                   + blender/ (bpy scripts, copied real)
content/           _schema (canonical closed vocab) + champions items runes maps minions balance cards i18n
assets-manifests/  hash->path manifests (blobs live in the content-addressed asset-store, not git)
services/ web/ infra/ docs/ tests/   scaffold dirs, filled during reconciliation
```

## Build
```
cargo build --workspace
cargo test  --workspace
```
Toolchain pinned to **1.96.1** (`rust-toolchain.toml`) to match the Bevy 0.20-dev
MSRV of the moon-lol fork.

## Migration
See [MIGRATION.md](./MIGRATION.md) — maps every existing branch/dir into its
destination crate/dir here. This is the start of the consolidation; laws
(determinism, closed-vocab, per-champion tests, no-binaries-in-git, original-IP,
server-authoritative) are inherited from the master plan.
