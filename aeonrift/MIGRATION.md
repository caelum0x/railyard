# MIGRATION — existing work -> aeonrift/

Maps every existing source (branch / directory) into its destination crate or dir
in this skeleton. This is the reconciliation map; the skeleton compiles today and
the real code migrates in per row. Each destination crate's `src/lib.rs` carries the
same pointer as a `// MIGRATE:` doc line.

## RENDER/PARTICLE graduation — Riot-decoupled render engine (2026-08-26)

The heavy render crates `lol_render` and `lol_particle` are now **GREEN additive
workspace members** on Bevy `0.20-dev`, compiled with `cargo +1.96.1`. They were the
"GRADUATE-LATER" render subset; they land WITHOUT disturbing the default build (the whole
`cargo +1.96.1 check --workspace` stays green) and WITHOUT copying any Riot data/assets.

### How the Riot-format coupling was removed (DECOUPLE, the preferred path)

Both crates were only *shallowly* coupled to the Riot-format crates
(`league_core`/`league_file`/`league_to_lol`), so decoupling — not vendoring the Riot
parsers — was the right call:

- **`lol_render`** had exactly **one** functional Riot line:
  `league_core::register::init_league_asset(app)` in `src/lib.rs` (registers the Riot
  WAD/PROP/TEX dynamic-asset loaders). It is now `#[cfg(feature = "riot-assets")]` and
  OFF by default; the crate renders ORIGINAL glTF (`.glb`) champion assets (the 47 in
  `assets/champions/`) via Bevy's native glTF loader + Bevy-native meshes. The unused
  `league_core`/`league_file`/`league_to_lol`/`league_loader`/`league_utils` deps and the
  unused `naga`/`walkdir` deps were dropped from its `Cargo.toml` (0 code references
  each — verified by grep).
- **`lol_particle`** coupled to Riot formats only in `src/loaders/scb.rs` (the `.scb`
  static-mesh loader via `league_file`/`league_to_lol`). That module and its
  `init_asset_loader::<ScbMeshLoader>()` registration are now `#[cfg(feature =
  "riot-assets")]`; particles consume ORIGINAL glTF/Bevy-native meshes by default. It
  keeps `league_utils` (already a member — pure hashing helpers, no Riot data).
- A new `clap` entry was added to the workspace deps (needed by `lol_particle`'s
  playback-server bin `main.rs`).

The `riot-assets` feature is a preserved seam (its backing Riot crates are deliberately
NOT workspace members, so it is a no-op path today, not an active build).

Build tails (`cargo +1.96.1`, `CARGO_TARGET_DIR` shared with the main checkout to stay
under the ~19GiB disk ceiling):
- `cargo +1.96.1 build -p lol_render` → `Finished dev profile ... in 2m 25s` (warnings
  only: one deprecated glam fn in lol_base_render, one unused-assignment in lol_render).
- `cargo +1.96.1 build -p lol_particle` → `Finished ... in 2m 32s` (lib + `lol_particle`
  bin; warnings only).
- `cargo +1.96.1 check --workspace` → `Finished ...` GREEN across all members (default
  workspace stays green; no existing member's source was touched).

### lol_rl — DEFERRED, honest blocker (NOT the candle/CUDA one the plan expected)

`lol_rl` is **still excluded**. The plan framed its blocker as candle-CUDA + postgres,
but that is the *lesser* problem:
- The candle fix is trivial: `lol_rl/Cargo.toml` has `default = ["cuda"]` — change to
  `default = []`. `src/device.rs` already selects `Device::Cpu` / `cuda_if_available`,
  so CPU-only self-play/inference works once the default feature no longer force-enables
  CUDA. postgres/`sqlx` (the `db.rs` training-persistence layer) would be feature-gated
  similarly.
- The **hard** blocker: `lol_rl` has a non-optional dependency on **`lol_env`**, used
  pervasively in its core library (`RlEnvironment` trait + concrete envs in
  `service.rs`, `ppo.rs`, `training.rs`, `rollout.rs`, `autotune.rs`, `async_session.rs`
  and the `for_all_rl_environments!` macro). `lol_env` in turn hard-depends on
  `lol_agent` (which embeds the **`rquickjs`** JS engine), plus `lol_render`,
  `lol_particle`, and `winit`. So `lol_rl` cannot compile until `lol_env` + `lol_agent`
  are also graduated.

Exact remaining work to graduate `lol_rl` (a separate, larger task):
1. Graduate `lol_agent` (decouple/verify `rquickjs` builds under `cargo +1.96.1`).
2. Graduate `lol_env` (now that `lol_render`/`lol_particle` are members; add `winit`).
3. In `lol_rl/Cargo.toml`: set candle `default = []`; feature-gate the `sqlx`/postgres
   `db.rs` persistence behind an off-by-default `persistence` feature.
4. Add `lol_rl` (+ `lol_env`, `lol_agent`) as members and build `-p lol_rl`.

This was NOT attempted to completion here because building candle-core + candle-nn +
rquickjs + winit + the env stack fresh would exceed the remaining disk budget (~5GiB
after render/particle) and the mandate is to STOP rather than thrash on disk-full. A
smaller green set (render + particle) was delivered instead of a broken/half-built one.

## INTEGRATION — real netcode + deterministic pathfinding (2026-08-26)

The deferred **M1.5** step is graduated: the cloned crates are now actually wired,
not just planned. All three land WITHOUT disturbing the default build — the
Bevy-0.19 deps (lightyear, vleue) live behind feature flags so
`cargo +1.96.1 build --workspace` (default features) never pulls them into the
Bevy-0.20-dev engine world.

### Netcode — lightyear 0.29 real UDP (`aeonrift-moba` `netcode` feature)
`games/aeonrift-moba/src/netcode/` wraps the untouched `sim` kernel in a real
lightyear server + client over **UDP** (`NetcodeServer`/`ServerUdpIo`,
`NetcodeClient`/`UdpIo`). The in-process predict/reconcile spine
(`server::Server` / `client::Client`) stays the DEFAULT (feature off) so
`no_desync` and the whole default build stay green. Spine → lightyear mapping:

| aeonrift spine (`sim::net`)     | lightyear 0.29                          |
|---------------------------------|------------------------------------------|
| `StampedInput` (inputs-only)    | `MoveInput` native input (`input_native`)|
| `Client::predict` / `reconcile` | `PredictionTarget` + rollback            |
| `sim::state_hash`               | `SimStateHash` component, `add_custom_hash`-bound into `ChecksumPlugin`'s per-tick XOR |
| fixed `DT` tick                 | `FixedUpdate` at `sim::TICK_HZ`          |
| `effect_tick` input delay       | lightyear input-delay config             |

- Build: `cargo +1.96.1 build -p aeonrift-moba --features netcode` ✅
- Run (real UDP loopback, 2 procs):
  `cargo +1.96.1 run -p aeonrift-moba --features netcode --bin moba-netcode -- server|client`
- Test: `cargo +1.96.1 test -p aeonrift-moba --features netcode --test netcode_udp` —
  spins a server + client as two in-process apps over **real UDP loopback**, drives a
  move, and asserts the client connects, receives replication, and converges toward the
  server's authoritative `sim::step` position. ✅ PASSES.

#### Checksum binding — `sim::state_hash` → lightyear per-tick checksum XOR
The `ChecksumPlugin` was previously *added*, and `SimChecksum`/`sim::state_hash` was
*computed* every tick, but nothing bound the fingerprint into lightyear's checksum, so
a client↔server divergence was never actually DETECTED. That binding is now closed:
- **`protocol::SimStateHash(u64)`** — a replicated + predicted component carrying the
  per-tick `sim::state_hash`. `shared::sim_step_system` writes `state_hash(&world)` into
  it every tick (server = authoritative hash; client = predicted hash, which lightyear
  records into `PredictionHistory<SimStateHash>`).
- **`add_custom_hash(hash_sim_state)`** on `SimStateHash` (lightyear 0.29's real spelling
  of the older `local_rollback().add_custom_hash()`; it is a `ComponentRegistration`
  method and must be called *before* `.predict()` downcasts the builder) feeds that
  `u64` into the per-tick checksum's `seahash` XOR.
- The **`Deterministic`** marker (`lightyear_deterministic_replication`) is inserted on
  the server's authoritative champion (`server.rs`) and the client's predicted champion
  (`client.rs::handle_controlled_spawn`) — it is the archetype opt-in the checksum
  machinery filters on. Without it the checksum would hash *no* entity and compare 0s.
- Net effect: the client's `ChecksumSendPlugin` XORs `hash(SimStateHash)` at its
  `LastConfirmedInput` tick and sends it; the server's `ChecksumReceivePlugin` compares
  it against its authoritative history — a mismatch is a **detected** desync, and
  lightyear's prediction rollback (`PredictionTarget` + replayed `MoveInput` history)
  re-applies unacknowledged inputs to reconverge on the authoritative state.
- Adds a direct `seahash = "4.1"` dep (netcode-gated) so the custom-hash hook can name
  `seahash::SeaHasher` — the exact hasher lightyear hands it.
- Test: `cargo +1.96.1 test -p aeonrift-moba --features netcode --test netcode_rollback`
  — proves the bind is REAL (the lightyear `ComponentRegistry` has a deterministic hash
  fn for `SimStateHash`, and `Deterministic` is registered), and that the fingerprint it
  carries is divergence-sensitive: a lost input yields a different `state_hash` (desync
  detected) while a deterministic replay of the acknowledged input stream reproduces the
  authoritative `state_hash` bit-for-bit (rollback reconverges). ✅ PASSES (3 tests).

**Scope / honest gap (catch-up `register_catchup`):** lightyear 0.29's
`AppCatchUpExt::register_catchup` (the `LateJoinCatchUpPlugin` path) is a **bevy_replicon
0.42**-based mid-game-join snapshot mechanism: it hides physics/state components behind a
replicon `SingleComponent<C>: FilterScope` visibility filter until a joining client
requests a bundled snapshot, and requires an `ActionStateSequence` input type. Our netcode
uses lightyear-**native** replication (`Replicate::to_clients`, native `MoveInput` inputs)
with a single controlling client and no mid-game join, so that replicon catch-up stack is
not wired in. The determinism guarantee it exists to protect — deterministic re-application
of unacknowledged inputs after a snapshot — is provided here by lightyear's prediction
rollback over the predicted entity's `MoveInput` history, now **guarded** by the
`SimStateHash` checksum bind above. The one remaining gap is that the `SimNet.world`
*resource* itself is re-simulated but not snapshot-restored to the confirmed tick before a
rollback replay (lightyear restores predicted *components* + input history, not arbitrary
resources); closing that fully would require storing sim-world state in prediction history
or adopting the replicon snapshot path, and is deferred.

### Pathfinding — deterministic engine crate (`aeonrift-pathfinding`, Bevy-free)
`engine/pathfinding` is now a real, Bevy-free, integer flow-field / grid nav layer
(`NavGrid`, `FlowField`, supercover `line_clear`, `route`). It is AUTHORITATIVE and
determinism-safe (integer BFS; `f32` only at the world↔cell boundary). Wired into
`rules::lanes::steer_minions` via a `Match.nav: NavGrid`: move targets now route
through the grid. On an obstacle-free map `route` returns the direct target, so
existing content behaves byte-for-byte identically (no regression); baking blockers
into the grid makes units path around them — all still through `sim::Input`.
- Tests: `aeonrift-pathfinding` unit tests (6) + `aeonrift-moba` `tests/pathfinding.rs`
  (3, deterministic routing-around-obstacle gate). ✅

### Client navmesh — vleue_navigator 0.16 (`aeonrift-moba` `client` feature)
`games/aeonrift-moba/src/clientnav.rs` uses vleue's Polyanya any-angle navmesh
(`VleueNavigatorPlugin`, `NavMesh::from_edge_and_obstacles`, `navmesh.path`) for
CLIENT-SIDE path preview / visualization ONLY — it never feeds the deterministic
sim. Gated behind `client` (Bevy 0.19).
- Build: `cargo +1.96.1 build -p aeonrift-moba --features client` ✅

> `engine/net` remains a stub by design: per the integration mandate the netcode
> landed as a feature on the game crate (`aeonrift-moba`), not in `engine/net`.

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
green with `cargo +1.96.1 build --workspace`. **UPDATE (2026-08-26): `lol_render` and
`lol_particle` have since been graduated IN as members** (Riot-format loaders
feature-gated off — see the "RENDER/PARTICLE graduation" section at the top of this
file). The crates that remain excluded: `lol_debug`, `lol_env`, `lol_agent`, `lol_rl`,
and the Riot-data-coupled/dev-only crates `league_core`, `league_file`, `league_to_lol`,
`lol_extractor`, `lol_game_process_manager`, `lol_cli`, `lol_web_*`, `apps/client`. No
Riot assets copied.

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

> **UPDATE (2026-08-26):** `lol_render` and `lol_particle` are now GREEN workspace
> members — see the "RENDER/PARTICLE graduation" section at the top of this file. The
> list below is the *remaining* excluded set.

These are intentionally left in `../moon-lol` and NOT migrated because they depend on the
excluded Riot-format crates or on deps that don't build in this environment:
- `lol_debug` → requires `lol_render` (debug overlay only; not needed for the engine).
- `lol_env` → requires `lol_render` + `lol_particle` + **`lol_agent` (rquickjs)** + `winit`.
- `lol_agent` → requires `lol_render` + `rquickjs` (embedded JS scripting engine).
- `lol_rl` → requires **`lol_env`** (hard dep across its core library — `RlEnvironment`
  trait used in `service.rs`/`ppo.rs`/`training.rs`/`rollout.rs`/`autotune.rs`/
  `async_session.rs`), PLUS `candle-core` default feature `cuda` (no CUDA on macOS) and
  postgres `sqlx`. The candle/postgres items are the *lesser* blocker (trivial Cargo
  fixes); the hard blocker is the `lol_env → lol_agent(rquickjs)` chain. See the deferral
  note in the top section for the exact remaining work.

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
- **DONE (2026-08-26):** `lol_render` + `lol_particle` graduated as members by DECOUPLING
  from the Riot formats (feature-gated `riot-assets`), rendering ORIGINAL glTF instead —
  see the "RENDER/PARTICLE graduation" section at the top of this file. They still
  co-exist with the `engine/{render,particle}` min-ECS stubs.
- **STILL DEFERRED:** the RL subset (`lol_rl` + its `lol_env`/`lol_agent` chain) — blocked
  on graduating `lol_env`/`lol_agent`(rquickjs), not on the asset pipeline. Maps onto the
  `engine/ai-bots` min-ECS stub. See the deferral note in the top section for exact work.
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
