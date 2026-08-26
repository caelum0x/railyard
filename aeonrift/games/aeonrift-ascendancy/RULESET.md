# Aeonrift: Ascendancy — Ruleset

The card-game ruleset **already exists in code** (it was not invented for this slice —
it lives in `src/rules/`). This document describes the implemented rules so they can be
read without reading the source. When the code and this doc disagree, the code
(`src/rules/*` + its tests) is authoritative.

Ascendancy is a **deterministic, headless, 1v1 lane card-battler** (Clash-Royale
shaped). It obeys the franchise determinism law it shares with the MOBA `sim`:
fixed tick, seeded RNG, ordered iteration, and a full-state hash. A whole match is a
pure function of `(MatchConfig, decks, deploy decisions)`.

## The reuse seam — a card *is* a champion

A `Card` (`src/cards/card.rs`) owns a `champions::ChampionSpec` verbatim — the exact
roster data the MOBA authors and duels with — plus one piece of card metadata, its
**elixir cost**. When deployed, the spec is compiled into a `sim::Champion` via
`champions::compile`, so the unit's HP / AD / armor / range / attack-speed / abilities
all come straight from the shared roster and every fight is resolved by the same
`sim` combat core the MOBA uses. **The card you play === the card you see**: the
playable card and its finished painterly face (`assets/cards/final-v2/<slug>.png`) are
tied by the champion's file **slug** (`src/cards/face.rs`).

### Elixir cost derivation (deterministic, integer-only)

`power = hp + ad*8 + armor*6`, then `cost = clamp(power / 300, 1, 10)`. Beefier
champions cost more. Pure integer math, so pricing is bit-identical on every machine.

## The board

- **Two players** (0 and 1), each with a **base / nexus** (a stationary
  `sim::Champion`). Player 0's base sits at `y = -40`, player 1's at `y = +40`.
- **Lanes**: 3 by default, evenly spread across `x ∈ [-20, 20]`. A deployed unit
  spawns just in front of its owner's base in the chosen lane and **marches toward the
  enemy base**.
- Geometry is a pure function of `MatchConfig` (`src/rules/lanes.rs`), identical in the
  server, the tests, and the client.

## Resources & cards

- **Elixir**: each player starts with `5`, gains `+1` every `45` ticks (~1.5 s at
  30 Hz), capped at `10` (`src/rules/elixir.rs`).
- **Deck / hand**: a deck is 8 champion cards (no duplicates, all from the roster —
  `src/deck/validation.rs`). In-match cards cycle Clash-style via a seeded draw pile
  and a rotating hand (`src/rules/deck.rs`, `src/rules/hand.rs`).

## The one player action — deploy

A deploy (`src/rules/deploy.rs`) is `{player, card index, lane}`, validated at the
boundary; every rejection is an explicit `DeployError`, never a silent no-op:

- `BadPlayer` / `BadCard` / `BadLane` — index out of range.
- `NotEnoughElixir { have, need }` — the economy rule, observable and testable.
- `MatchOver` — the match already resolved.

On success it spends the elixir, spawns the unit into the shared `sim::World` with
ascending entity ids (so ordering — and the hash — stays stable), and points the unit
at the enemy base.

## One tick (the fixed pipeline)

`step_tick` (`src/rules/runner.rs`) — same shape as `sim::step`:

1. **regenerate elixir** (on the fixed period),
2. **apply this tick's deploys** in order,
3. **compute unit movement intents** in ascending id order (march toward the enemy
   base, or hold to fight an enemy in range — `src/rules/unit_ai.rs`),
4. **advance the shared `sim` world one tick** (movement, combat, projectiles, ability
   resolution, death),
5. **evaluate the win condition**.

Same seed + same deploy decisions ⇒ byte-identical state and `match_hash` every run.

## Combat & abilities

Units and bases are `sim::Champion`s inside one `sim::World`, so the sim's own
auto-attack, projectile, mitigation, and ability systems resolve every fight. A
champion's abilities come from its RON spec through the ability DSL (M5); armor
mitigation, attack cadence, and ability cooldowns are all the sim's.

## Win condition

Destroy the enemy base to win (`src/rules/win.rs`). Both bases falling on the same
tick is a **draw**; reaching the tick cap (`max_ticks`, default 20 000) with both bases
standing is also a draw.

## Determinism & the state hash

`match_hash` (`src/rules/match_state.rs`) folds the full `sim` world hash together with
the card layer's own state (tick, elixir, unit-id source, deploy counts, outcome). It
is the regression fingerprint: two matches with identical content produce an identical
hash, and any drift is caught by the tests in `tests/determinism.rs`.

## Modes, bot, and the playable-slice tooling

- **Bot** (`src/ai/deploy_bot.rs`): a pure, deterministic opponent policy (Easy /
  Normal / Hard) — used by PvE, the runner, and the tests as a reproducible opponent.
- **Modes** (`src/modes/`): ladder / draft / pve configs + the PvE match driver.
- **`cardmatch` bin**: runs a scripted bot-vs-bot match and prints the result + hash
  (the deterministic green/red gate).
- **`matchreplay` bin** (this slice): records a deterministic match into per-beat board
  snapshots (`src/rules/replay.rs`) and renders them to HTML — `REPLAY.html` (an
  interactive, windowless replay), `BOARD.html` (a static board/HUD mockup), and
  `turn_00..NN.html` frames — all drawn with the **real finished card faces**.
- **`cards-client` bin** (behind the `client` feature): the Bevy 0.19 touch client, a
  thin VIEW over this same rules core.
