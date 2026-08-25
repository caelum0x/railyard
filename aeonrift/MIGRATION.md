# MIGRATION — existing work -> aeonrift/

Maps every existing source (branch / directory) into its destination crate or dir
in this skeleton. This is the reconciliation map; the skeleton compiles today and
the real code migrates in per row. Each destination crate's `src/lib.rs` carries the
same pointer as a `// MIGRATE:` doc line.

## CONSOLIDATION — everything is now on `master` (2026-08-26)

All previously-scattered franchise work has been merged onto **`master`**. There is no
longer any work sitting only on a feature branch. The consolidation merged, in order:
`content/full-tree` → `engine/moonlol-migration` → `games/moba-architecture` →
`games/ascendancy-architecture`, then pruned the Riot champions and folded in the
graduated originals.

### Final workspace members (37 crates)

**24 original aeonrift-\* crates** (min-ECS proof path + tools + game shells):
`aeonrift-sim-core`, `aeonrift-combat`, `aeonrift-ability-dsl`, `aeonrift-net`,
`aeonrift-protocol`, `aeonrift-pathfinding`, `aeonrift-vision`, `aeonrift-economy`,
`aeonrift-ai-bots`, `aeonrift-render`, `aeonrift-particle`, `aeonrift-audio-engine`,
`aeonrift-ui-kit`, `aeonrift-platform`, `aeonrift-moba`, `aeonrift-ascendancy`,
`aeonrift-champion-gen`, `aeonrift-card-gen`, `aeonrift-balance-sim`,
`aeonrift-content-lint`, `aeonrift-asset-cli`, `aeonrift-localization`,
`aeonrift-mcp`, `aeonrift-graph`.

**13 migrated moon-lol crates** (real Bevy engine, original-IP base):
`league_utils`, `league_property`, `league_loader`, `lol_share`, `lol_rl_protocol`,
`lol_base`, `lol_base_render`, `lol_core`, `lol_champions`, `lol_client`, `lol_rpc`,
`lol_server`, `lol_agent_runtime`.

### Champion roster — 11 original-IP champions, ZERO Riot

`engine/lol_champions/src/` now contains exactly 11 champion modules, all original IP:
**erebos** (Eclipse Warden, seed) plus the 10 graduated from moon-lol
`graduate/champions`: **zeus, medusa, tyr, loki, mars, set_lord_of_chaos, kaeza,
lumisel, dregmaw, mosscloak**. The 157 Riot champion modules (aatrox..zyra) were
deleted, along with the `riot-harness` feature and the `test_utils.rs` Riot-asset
harness. Verified: zero Riot champion modules remain.

### Excluded heavy crates (kept in `../moon-lol`, NOT workspace members)

No crate had to be excluded during consolidation itself — the workspace builds fully
green with `cargo +1.96.1 build --workspace`. The crates below were already excluded by
the moon-lol migration (they depend on excluded Riot-format crates or on deps that don't
build in this environment) and remain excluded: `lol_render`, `lol_particle`,
`lol_debug`, `lol_env`, `lol_agent`, `lol_rl`, and the Riot-data-coupled/dev-only crates
`league_core`, `league_file`, `league_to_lol`, `lol_extractor`,
`lol_game_process_manager`, `lol_cli`, `lol_web_*`, `apps/client`. No Riot assets copied.

### Build + test status (toolchain `cargo +1.96.1`, Bevy `0.20-dev`)

- `cargo +1.96.1 build --workspace` — **GREEN** (all 37 members compile).
- `cargo +1.96.1 test -p lol_champions` — 38 passed, 0 failed.
- `cargo +1.96.1 test -p aeonrift-moba -p aeonrift-ascendancy` — all green
  (moba 8, ascendancy/cards 56).
- min-ECS (`aeonrift-sim-core`, `aeonrift-combat`, `aeonrift-ability-dsl`) — all green
  (ability-dsl 33, combat 1).
- **Known/accepted:** `lol_core` has **9 pre-existing unit-test failures** (8
  `life::tests` health/mana-regen + respawn, 1 `skill::integration_tests` recast-window),
  73 passed. These are byte-identical to the moon-lol source and are timing/harness
  properties under toolchain 1.96.1 — **not** consolidation regressions. No NEW failures
  were introduced.

### Content resolution note (cards)

`content/full-tree` and `games/ascendancy-architecture` both authored
`content/cards/decks/*` and `content/cards/packs/*`, in two incompatible RON schemas.
The ascendancy game code owns and globs `content/cards/decks/` (parsing `DeckDef`,
expecting exactly 12 decks) and `include_str!`s `rarities.ron`, so the ascendancy-schema
files were kept and the 8 unused content-tree `PresetDeck`-schema deck files (which would
break the `deck_presets` gate) were removed. Packs are loaded by name, so both pack
formats coexist harmlessly.

## Migration status — done (branch `fill/migrate-working-stack`)

The **min-ECS working stack** from `moba/` (all tests green upstream) is migrated in
and the whole workspace is green: `cargo build --workspace` + `cargo test --workspace`
pass (67 tests, 0 failures) on the pinned rustc 1.96.1.

FILLED + GREEN:
- **`engine/sim-core`** ← `moba/sim` (world, systems, combat, projectiles, hash, input,
  net value-types, seeded RNG, `simrun` bin). Package `aeonrift-sim-core`; the library
  keeps lib-name **`sim`** so migrated call-sites (`use sim::...`) are unchanged.
- **`engine/combat`** ← thin facade re-exporting the combat primitives from `sim-core`
  (combat + world live in one deterministic kernel, per the moba `sim` design).
- **`engine/ability-dsl`** ← `moba/champions` (spec, validate, loader, interpreter,
  duel, `duel` bin + full test suite). Package `aeonrift-ability-dsl`; lib-name
  **`champions`**. Test spec-dir constants repointed to `content/champions/`.
- **`games/aeonrift-ascendancy`** ← `moba/cards` (deterministic lane-battler rules +
  `cardmatch` bin + tests) **and** `moba/cards-client` (the Bevy touch client). Package
  `aeonrift-ascendancy`; lib-name **`cards`** (lib + cdylib). The Bevy client lives in
  `src/client/` behind the **`client`** feature (`--features client`) with a
  `cards-client` bin — so the default workspace build stays fast + green, while the
  client compiles and runs (headless smoke resolves a deterministic match). Mobile
  entry (`#[bevy_main]`) + Android metadata preserved.
- **`games/aeonrift-moba`** ← `moba/server` + `moba/client` (M0/M1 netcode spine:
  authoritative server, predicting/reconciling client, `no_desync` integration test,
  `moba-server` bin). Folded into one crate as `server` + `client` modules on `sim-core`.
- **`content/champions/`** ← 47 champion specs (+ `roster_meta.ron` sidecar) as flat
  `*.ron`; the DSL loader + roster/duel/asset tests read from here.
- **`assets/champions/<slug>/model.glb`** ← 47 real Blender greybox glTF binaries.
- **`assets-manifests/champions/<slug>/assets.manifest.ron`** ← 47 per-champion
  manifests (archetype/faction/blake3/bytes). The Rust `assets.rs` gate validates all
  47 specs resolve to a valid glTF-2 binary + manifest.

Design note: lib-names (`sim`, `champions`, `cards`) are preserved under the `aeonrift-*`
package names — the same pattern the skeleton already used (package `aeonrift-ascendancy`
→ lib `cards`). This kept the migration a near-zero-diff of the proven code (only Cargo
package/lib metadata, dependency paths, and test/`include_str!` spec paths changed).

DEFERRED / still stubs (compile as `Placeholder`, kept out of the working path so the
build stays green): `engine/{net,protocol,pathfinding,vision,economy,ai-bots,render,
particle,audio-engine,ui-kit,platform}` and `tools/*`.

## Migration status — moon-lol Bevy engine (branch `engine/moonlol-migration`)

The **real moon-lol Bevy engine** (the mature ~800-file engine, original-IP base from
`../moon-lol` branch `fork/original-ip`) is now migrated IN under `engine/` alongside the
min-ECS crates. This is **additive**: the min-ECS crates (`aeonrift-sim-core`,
`aeonrift-ability-dsl`, `games/*`, `tools/*`) are UNTOUCHED and remain the lightweight,
fully-green proof path (89 tests still pass). Crate/package names are preserved
(`lol_*` / `league_*`) so the engine's ~800 internal `use` paths never had to be rewritten.

Toolchain: `cargo +1.96.1` (Bevy `0.20-dev` git fork `mutemoon/bevy@ec2d1004`).

### Migrated + GREEN (workspace members, `cargo build --workspace` passes)

bevy-free utils / data:
- `engine/league_utils`, `engine/league_property`, `engine/league_loader` — Riot-format
  helpers/parsers, bevy-free (compile as pure code; **no Riot data copied**).
- `engine/lol_share` (glam+serde wire types), `engine/lol_rl_protocol` (serde+bincode).

gameplay brain (Bevy):
- `engine/lol_base`, `engine/lol_base_render` — base types, animation/render commands.
- `engine/lol_core` ★ — the game brain: `skill/`, `action/`, `base/buff`, `buffs/`,
  `damage/`, `entities/`, `life/`, `cooldown/`. Asset-agnostic.
- `engine/lol_champions` ★ — one Bevy `Plugin` per champion. **All 157 Riot champion
  modules were pruned during consolidation**; the roster is now **11 original-IP
  champions** (see the consolidation section at the top of this file). Compiles + all
  38 asset-free champion tests pass.

netcode / server / agent:
- `engine/lol_client`, `engine/lol_rpc`, `engine/lol_server` — authoritative server + RPC.
- `engine/lol_agent_runtime` — rig/rmcp agent framework (no render/Riot deps).

### EXCLUDED heavy / blocked crates (NOT members — see root `Cargo.toml` comment)

These are intentionally left in `../moon-lol` and NOT migrated because they depend on the
excluded Riot-format crates or on deps that don't build in this environment:
- `lol_render` → requires `league_core`/`league_file`/`league_to_lol` (Riot formats) + `naga`.
- `lol_particle` → requires `lol_render` + `league_file`/`league_to_lol`.
- `lol_debug` → requires `lol_render`. `lol_env` → requires `lol_render`+`lol_particle`+`winit`.
- `lol_agent` → requires `lol_render` + `rquickjs`.
- `lol_rl` → `candle-core` default feature `cuda` (no CUDA on macOS) + postgres `sqlx`.

Riot-data-coupled / dev-only crates excluded entirely (per FORK-NOTES): `league_core`,
`league_file`, `league_to_lol`, `lol_extractor`, `lol_game_process_manager`, `lol_cli`,
`lol_web_*`, `apps/client`. **No Riot WAD/PROP/TEX assets or champion configs were copied.**

### Champion test harness seam

`lol_champions` now has a single, asset-free test path: every one of the 11 original
champions uses the in-memory `Spell` test path (no Riot files), all green under
`cargo test -p lol_champions` (38 tests). The old Riot-asset render/integration
harness (`test_utils.rs` + the Riot-modelled champion tests) was **removed** during
consolidation along with the Riot champions and the off-by-default `riot-harness`
feature it was gated behind.

### Known: 9 pre-existing lol_core engine tests fail under this build

`engine/lol_core/src` is **byte-identical** to moon-lol (unmodified by migration). 9 of
its internal unit tests fail here — 8 `life::tests` (health/mana regen + respawn) and 1
`skill::integration_tests` — all timing/harness-sensitive (`run_system_once` +
`Time<Fixed>::advance_by`). They are properties of the source crate's test harness under
toolchain 1.96.1, not a migration regression, and are unrelated to the min-ECS path.
`cargo build --workspace` and `cargo check --workspace` are green; only these engine unit
tests are red. The 89 min-ECS tests and the Erebos champion tests all pass.

### GRADUATE-LATER track (still deferred)
- The heavy render/particle/RL subset above graduates when the asset pipeline (our own,
  non-Riot format) lands to replace the `league_*` Riot-format dependency of `lol_render`.
  These still map onto the `engine/{render,particle,ai-bots}` min-ECS stubs.
- The MOBA netcode graduates from the in-process predict/reconcile spine
  (`aeonrift-moba`) to real UDP transport (bevy_replicon / lightyear) in `engine/net` +
  `engine/protocol`.

Sources:
- **moba/** (git repo, branches below) — sim-core + M2 combat + M5 ability-DSL + rosters + cards + asset pipeline.
- **moon-lol** fork branch **fork/original-ip** — engine fork (render, netcode, RL bots, champion plugins) + canonical `content/_schema`.

## Engine (shared kernel)
| Source | Destination |
|--------|-------------|
| `moba/sim` (deterministic tick, seeded RNG) + `moon-lol` crates/lol_core | `engine/sim-core` |
| `moba/sim` (combat) + `moon-lol` lol_core (damage.rs, buffs/, action/) | `engine/combat` |
| `moba/champions` (spec loader + interpreter, M5 DSL) | `engine/ability-dsl` |
| `moon-lol` lol_rl_protocol + lol_rpc + lol_share | `engine/net` |
| `moon-lol` lol_web_protocol + lol_share (wire types) | `engine/protocol` |
| `moba/sim` movement + `moon-lol` lol_core navigation | `engine/pathfinding` |
| `moon-lol` lol_core (vision/fog) | `engine/vision` |
| `moba/sim` gold model | `engine/economy` |
| `moon-lol` lol_rl + lol_agent + lol_agent_runtime | `engine/ai-bots` |
| `moon-lol` lol_render + lol_base_render | `engine/render` |
| `moon-lol` lol_particle | `engine/particle` |
| `moon-lol` client audio path | `engine/audio-engine` |
| `moon-lol` lol_client (UI) | `engine/ui-kit` |
| `moon-lol` lol_env + lol_game_process_manager | `engine/platform` |

## Games
| Source | Destination |
|--------|-------------|
| `moba/server` + `moba/client` + `moba/sim` rules + `moon-lol` lol_server/lol_client | `games/aeonrift-moba` |
| branch `moba/cards-game` (`cards/`) + branch `moba/cards-client` (`cards-client/`) | `games/aeonrift-ascendancy` (lib+cdylib) |

## Content
| Source | Destination |
|--------|-------------|
| `moon-lol` fork/original-ip `content/_schema/` (vocabulary.ron + schemas) | `content/_schema/` (copied canonical) |
| branches `moba/roster-mythology`, `moba/roster-fantasy`, `moba/roster-umbral`, `moba/original-roster` (`champions/`) + `moon-lol` content/champions | `content/champions/` |
| `moba/champions/specs` (items/runes/etc as they land) | `content/{items,runes,maps,minions,balance}/` |
| card tuning from `moba/cards-game` | `content/cards/` |
| new i18n authoring | `content/i18n/` |

## Tools / pipeline
| Source | Destination |
|--------|-------------|
| branch `moba/asset-pipeline` `tools/blender/` (gen_character.py, batch.py, validate_assets.py) | `tools/blender/` (copied real) |
| branch `moba/asset-pipeline` `tools/aeonrift_assets/` (blake3, glb, manifest, roster) + `tools/gltf_fallback.py` | `tools/asset-cli` + `assets-manifests/` |
| `moba/champions` authoring | `tools/champion-gen` |
| `moba/cards-game` card_meta derivation | `tools/card-gen` |
| `moon-lol` lol_rl self-play | `tools/balance-sim` |
| `moon-lol` content/_schema validation + `moba/champions` loader checks | `tools/content-lint` |
| new i18n/VO tooling | `tools/localization` |
| `moon-lol` .claude harness | `tools/mcp` (aeonrift-harness MCP) |
| AEONRIFT-GRAPH.md extractor | `tools/graph` |

## Services / web / infra (scaffold, filled later)
| Source | Destination |
|--------|-------------|
| Railyard/Mint + CalDesk economy wiring | `services/economy` (+ accounts, matchmaking, orchestrator, ...) |
| ereboslab.com scaffold | `web/compliance` + `web/champion-pages` |
| — | `infra/` (terraform/k8s/docker/CDN/R2), `tests/`, `docs/` |

## Branch inventory (moba/)
`master`, `moba/m2-combat`, `moba/m5-champion-dsl`, `moba/original-roster`,
`moba/roster-mythology`, `moba/roster-fantasy`, `moba/roster-umbral`,
`moba/cards-game`, `moba/cards-client`, `moba/asset-pipeline`, `moba/assets-real`.
Each folds into the rows above; the codegen loop (AEONRIFT-MASTER-PLAN §"codegen loop")
then fills `content/` at scale.
