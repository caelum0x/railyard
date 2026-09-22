# Railyard — "DONE FULLY" Plan

_Grounded read-only audit captured 2026-08-28 on branch `mint/devserver-integration-harness`
(working tree intentionally dirty, ~220 modified files, mid-WIP). No code was changed to
produce this document. "DONE FULLY" here means **shippable**, not bigger — see §5._

> **Scope note.** This repo is ~770k LOC but that number is misleading: **~535k of it is the
> vendored TensorZero engine inside `apps/relay/`** and a further large share of `aeonrift/` is a
> migrated League-clone Bevy engine. The **owner-written** surface is far smaller (mint ≈23k,
> vend ≈15k, aeonrift games ≈10k, etc.). Effort should be sized against the owned code, not the
> headline LOC.

---

## 1. What Railyard actually is (two halves)

Railyard is **two unrelated products sharing one git repo**:

### Half A — the settlement + model rail (`apps/`, ~691k LOC, mostly vendored)
The primary product per `README.md`. A set of independent Rust workspaces that together form an
**AI-payments rail**. The durable asset is **Mint**: a fiat-backed, gasless, success-only
microdollar ledger with hash-linked Ed25519 receipts and a payment handshake protocol (AITP-402).
Everything else either rides Mint or installs it under a new class of payer:

| Crate | Plain-terms role | Path |
|---|---|---|
| **mint** | The ledger. Double-entry, reserve-conservation, signed hash-linked receipts, on-ramp checkouts (Stripe/Creem/Polar/PayPal/USDC/iyzico), KYC-tiered redeem. The core asset. | `apps/mint/` (11 crates) |
| **relay** | Model gateway — a **TensorZero fork** — with success-only Mint billing bolted on. The "model rail." | `apps/relay/` (owned overlay: `gateway`, `relay-billing`, `relay-hedge`) |
| **agent-runtime** | Shared AITP-402 payment SDK substrate consumed by turnstile + abacus (ed25519 signing, paid-inference decorator, relay provider client). | `apps/agent-runtime/` (`agent-core`, `aitp-client`, `relay-provider`) |
| **vend** | "Stripe-meets-OpenRouter": a resellable metered AI SDK on the rail, with settlement + hold-reaper + reconciliation. | `apps/vend/` (9 crates) |
| **abacus** | Inline proxy enforcing hard, receipted per-agent spend budgets (kill-switch), Postgres-backed policy/hold recovery. | `apps/abacus/` (5 crates) |
| **turnstile** | Drop-in agent paywall middleware ("Stripe for AI agents"), Postgres challenge + settlement journal and leased recovery. | `apps/turnstile/` (`turnstile-core/axum/demo`) |
| **identity-lock** | Provider-agnostic locked character/brand/persona identity, resolved through Relay. Source path: `apps/context-passport/`. | `apps/context-passport/` (9 crates) |
| **ledgerline** | Gross-margin lens: revenue + provider COGS on one Mint-backed ledger, settled through a durable outbox saga. | `apps/ledgerline/` (1 crate) |

**Adjacent tooling in `apps/` (not the settlement rail itself):**
- **game-economy** (`apps/game-economy/`, 4 crates) — a storefront for card packs / cosmetics / battle passes on the rail (bridges Half A ↔ Half B economically).
- **rit** (`apps/rit/`) — a library-first, real-Git-interoperable reimplementation of Git's local object model. Mature local slice.
- **rithub** (`apps/rithub/`) — architecture-only forge on top of `rit`. **Docs only, no code.**
- **ranguard** (`apps/ranguard/`) — a userspace "honest anti-cheat" demo/parody (signed integrity manifests, process attestation). Adjacent demo, not rail.

Consumers (Menivor / `../videogen`) call these over HTTP + shared contracts; they do **not** own them.

### Half B — Aeonrift (`aeonrift/` + `curation/` + `slice/` + `train/`, ~85k LOC)
A completely separate **dark-fantasy game franchise + AI-art pipeline**, original-IP-decoupled from a
League-of-Legends Bevy fork:
- **`aeonrift/`** — a Rust/Bevy monorepo: a shared engine (`engine/`), two games
  (`games/aeonrift-moba` 5v5 + `games/aeonrift-ascendancy` mobile card-battler), tools, and closed-vocab content.
- **`curation/`** — 47 curated champion splash JPG anchors (the art review set).
- **`train/`** — 22 uniform caption `.txt` files feeding a **style LoRA** (`LORA.md`).
- **`slice/`** — a Blender + headless-Bevy toon/turntable rendering slice (mostly build artifacts on disk).

The two halves share only the repo and the `game-economy` monetization bridge. **They are otherwise
independent and should be planned as separate tracks.**

---

## 2. Completeness per crate / area

> **KEY FINDING — the top-level docs are stale and pessimistic.** `README.md` and
> `docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md` list several "open gaps" that **this WIP branch has
> already closed.** The current dirty tree is materially *ahead* of the committed status labels.
> Verified directly:
> - README "abacus: single-process" → **Postgres policy/hold store implemented** (`apps/abacus/crates/abacus-proxy/`, capability-graph G3 = DONE, commit chain).
> - README "turnstile: SDK thin / single-process" → **durable Postgres challenge ownership plus a settlement journal and leased recovery shipped**.
> - README "relay: needs price catalog" → **per-model catalog implemented**: `apps/relay/crates/relay-billing/src/catalog.rs` (compile-time price table) + `catalog_config.rs` (operator file overrides), namespaced `openrouter/{id}`.
> - README "vend: needs crash reaper" → **hold-reaper implemented**: `apps/vend/crates/vend-settlement/src/reap.rs` + `vend-store/src/reconciliation.rs` (leased, TTL-bounded, capture-safe).
> - README "ledgerline: non-atomic settle" → **durable outbox saga implemented**: `apps/ledgerline/src/settle.rs` + `outbox.rs` ("atomic-or-compensated, never half-settled").
>
> Treat the table below (from live code), not the README badges, as current truth.

### Half A — settlement rail

| Crate | Status | Evidence | Notable gaps / risk |
|---|---|---|---|
| **mint** | **DONE (ship-ready core)** | 11 crates, 49 test files, 9 SQL migrations (`mint-store-postgres/migrations/0001..0009`), double-entry + conservation invariants documented (`mint-ledger/src/ledger.rs`, `allowance.rs`), deny-by-default KYC (`mint-core/src/error.rs`, commit `e0157b7`), 0 live `TODO/todo!()`. Refuses dev signer when Postgres configured. | No `Dockerfile`/deploy manifest. No control-plane (authenticated key rotation), no multi-currency, no custody/anchoring. Design docs referenced in README (`docs/mint/PLAN.md`) are **missing from this repo** (doc drift — likely left in videogen monorepo). |
| **relay** (owned overlay) | **WIP → paid inference path DONE** | TensorZero provider dispatch directly uses `relay-hedge` for opt-in buffered/first-token racing, progressive stagger deadlines, adaptive SLO routing, circuit breaking, spend caps, stored config, and Prometheus metrics. Provider-health EWMAs and circuit state are restart-durable Postgres checkpoints with downtime-aware cooldown restoration. Native inference, OpenAI chat/completions/responses/embeddings, gateway-sized multimodal requests, and asynchronous batch start/poll use durable success-only Mint settlement. Paid batches persist principal ownership and one poller lease, retain the hold while pending, release on failure, and capture aggregate usage on completion. Refund/recovery jobs are graceful-shutdown tracked. 33 hedge tests, 87 billing-core tests, 37 focused gateway billing tests, 6 outbox tests, and the gateway persistence round-trip pass. | Paid batch reconciliation requires the aggregate batch poll; partial per-inference polling is intentionally rejected because it cannot prove the aggregate charge. Catalog accuracy remains an operator responsibility. Vendored TensorZero TODOs are not owned Relay gaps. The build script reuses hash-verified sibling TypeScript/SES artifacts offline; a completely clean machine still needs pinned asset paths or an initial network fetch. |
| **agent-runtime** | **WIP → paid runtime usable** | `aitp-client` is the proven composition primitive (turnstile + abacus consume it), with streaming receipt recovery, `StubPaymentGateway`, and real `MintPaymentGateway`. Relay-backed buffered/SSE requests use Relay-owned dynamic token settlement. The separate direct-Mint compatibility decorator now meters buffered and incremental fixed-price Rig calls, reconciles early drop, releases non-terminal failures, and never flips ambiguous capture to release. | Multi-turn/tool session policy and a durable client-side settlement journal remain deferred. The direct compatibility path is intentionally fixed-price; dynamic token pricing belongs to Relay. Needs rustc ≥1.94. |
| **vend** | **DONE (mature service)** | 9 crates, 8 test files, `Dockerfile` present, settlement + reconciliation + reap. README calls it "very mature." | Hand-rolled `vend-mint-client`/`vend-relay-client` duplicate mint/relay SDKs (dedupe debt). |
| **abacus** | **WIP → mostly DONE** | 5 crates, Postgres store + exclusive recovery leases + transactional receipt projection (capability-graph "ABACUS SLICE DONE"). Hold-capture clamped to reserved ceiling (commit `aad9583`). | Local-JSON fallback is explicitly single-replica; only replica-safe when `DATABASE_URL` set. |
| **turnstile** | **WIP → durable core DONE** | 3 crates; exact issued challenge ownership; atomic nonce consume; journaled `verifying → held → capture_pending/captured` or `release_pending/released`; owner leases + heartbeats; atomic Postgres recovery claims; signed Mint receipt preservation; Axum fails closed on capture ambiguity. | Provider registration/discovery, settlement-status API, non-Axum adapters, MCP helper, and published SDK packages remain. Live multi-process Postgres + Mint acceptance still needs an available external environment. |
| **identity-lock** (`context-passport`) | **WIP (runnable Sx0)** | 9 crates incl. `passport-store-postgres` + `passport-testkit`, full axum API + Postgres migrations + provenance, 38 test markers. | Dir name is legacy — shipped product is **Identity Lock**; the OAuth "Context Passport" concept in its `PLAN.md` is **not implemented**. Video identity only; live provider conditioning, artifact minting, Ed25519/Merkle anchoring, real C2PA JUMBF, grants ACL deferred. |
| **ledgerline** | **WIP (logic done, untested at scale)** | Outbox saga implemented (`settle.rs`, `outbox.rs`, `PostgresOutbox`), rich inline fault-injection; correctly treats a captured hold as terminal → compensates via **reverse-transfer, never release**. | **Money-path flag:** `settle()`'s outbox upsert uses `ON CONFLICT` with **no `FOR UPDATE` row lock**, so two concurrent settles for one `request_id` can both run forward (Mint per-step idempotency prevents double-charge, but interleaving is possible — _inferred_). **`tests/` dir is empty** (all inline); 1M-event scale test deferred. Single crate vs 9-crate enterprise target. |
| **game-economy** | **WIP (library only)** | 4 crates (`-core/-mint-client/-store/-testkit`), 47 test markers, idempotent-by-construction entitlements (deterministic `ent_…` id), rejects self-synthesized receipts, idempotent single-owner retry fixed (commit `4db2c07`). | **No server/binary crate** — it's an embeddable library; `JsonFileGameState` is single-process, no shipped DB `GameStateRepository`. Not a standalone deployable service. |
| **rit** | **DONE local + safe one-ref push slice** | 89 tests (56 unit + 33 real-Git interop), atomic index.lock/ref CAS, strict pkt-line + receive-pack vocabulary, gitoxide-backed full/thin pack verification, quarantined decoded objects, bounded connectivity/fsck, fast-forward policy, stock-Git push/force-refusal/loose-delete journeys, packed-ref deletion refusal, clone/incremental-fetch through an explicit installed-Git upload-pack bridge, and stock-`git fsck --strict` verification. | Native Rust upload-pack/fetch, Smart-HTTP/SSH listener integration, multi-ref atomic receive, packed-refs rewrite/deletion, interrupted-ingest recovery, merge, and SHA-256 remain (capability-graph G9). |
| **rithub** | **STUB** | `README.md` + `ARCHITECTURE.md` + `RUST-GIT-FORGE-PLAN.md` only. No `Cargo.toml`, no code. Explicitly gated until rit protocol v2. | Entire product unbuilt (by design). |
| **ranguard** | **DEMO (real but adjacent)** | Single crate, 5 test files, userspace sandbox integrity monitor. | Parody/demo; not part of the rail's money path. |

**Money-path correctness summary (the part that actually matters):** the invariant-critical code —
Mint double-entry/conservation/receipt chain (PG `FOR UPDATE` locks taken in `AccountId` sort order,
documented deadlock-free; supply conserved by construction; `gate_million` scale tests), abacus hold
clamp + `SKIP LOCKED` lease recovery + reaper, vend plan-before-capture + per-leg idempotency keys +
orphan reaper, ledgerline atomic-or-compensated outbox saga, and Turnstile's atomic nonce consume +
monotonic leased settlement recovery — is
**implemented and test-backed**. Two **residual money-path caveats** to carry forward (none is a
double-charge bug; Mint owns idempotency): (1) ledgerline `settle()` takes **no row lock** during its
outbox upsert (concurrent settles can interleave — _inferred_) and ships an **empty `tests/` dir**;
(2) agent-runtime's direct-Mint compatibility decorator is deliberately fixed-price; dynamic token
usage settlement lives in Relay. The dominant remaining
risk is **integration-level + durability-under-failure**, not missing logic. See §3.

**CI reality:** _no railyard-native crate has its own CI_ — the only `.github/workflows` in the tree
are the ~40 **vendored TensorZero** workflows under `apps/relay/`. Every "tests pass" claim for mint/
vend/abacus/etc. is from local runs (capability graph), not a pipeline. Standing up per-app CI with a
Postgres service is itself a ship-blocker (R1).

### Half B — Aeonrift (corroborated by deep sub-audit)

| Area | Status | Evidence |
|---|---|---|
| Engine — migrated `lol_*`/`league_*` Bevy layer | **DONE (large, real)** | `lol_champions` 17.4k LOC, `lol_core` 14.4k, `lol_render` 6.3k, `lol_particle` 3.7k, etc. Riot loaders feature-gated OFF (`riot-assets`). This is the real engine. |
| Engine — native `engine/*` consolidation crates | **MIXED** | Real: `sim-core` (1.1k, the deterministic kernel), `ability-dsl` (1.9k), `pathfinding` (542), `audio-engine` (380). **STUB (~33 LOC each):** `net`, `protocol`, `render`, `vision`, `particle`, `platform`, `ui-kit`, `economy`, `ai-bots`. `combat` is a **facade** re-exporting sim-core. (README's "compiling skeleton / `// MIGRATE:`" language is **stale** — 0 MIGRATE markers, 0 `unimplemented!()`, 1 `todo!()` remain.) |
| **games/aeonrift-ascendancy** (mobile cards) | **WIP — genuine playable vertical slice (most complete game)** | 6.4k src LOC, 4 bins (`cardmatch` bot-vs-bot gate, `matchreplay`, `cards-client`, `client-frames`), full `rules/deck/modes/ai/server/client` (Bevy touch screens), mobile packaging (`mobile/ios/Info.plist`, cargo-apk metadata), committed proof frames (`replay/*.png`). 8 real integration tests incl. `match_win.rs` (asserts decisive win). "A card IS a champion" → compiles through shared `sim`. |
| **games/aeonrift-moba** (5v5) | **WIP (thinner, real netcode spine)** | 3.6k src LOC, authoritative server + lightyear UDP netcode + rules (turrets/waves/jungle/objectives) + laning bot. Bins `moba-server`, `moba-netcode`. Tests: `moba_match`, `netcode_rollback`, `no_desync`, `netcode_udp`, `pathfinding`. No screen-level client UX yet. |
| Content | **DONE-enough for slice** | 48 champion `.ron` specs + 47 ability subdirs, 24 preset decks, 35 packs, 303 items, 122 runes, closed-vocab `_schema`. Real authored data, not schema-only (some item/rune bulk likely imported from League migration — spot-check advised). |
| Art / LoRA pipeline | **Style LoRA DONE; 3D chain proven for 1 champion** | `LORA.md`: shipped versioned Replicate model `caelum0x/aeon-style-lora:433a51a0…` (flux-dev + LoRA, trigger `AEONSPLASH`, H100 job `ywga6j…`, final loss ~0.3–0.4). 3D chain (flux-dev → TRELLIS→ Hunyuan3D-2.1 → Blender EEVEE → toon/turntable) validated **end-to-end for Anubis only**. Tools under `aeonrift/tools/{cardgen,aeonrift_models_ai,aeonrift_3d_hq,aeonrift_3d_ab,aeonrift_matte_v2}` + `slice/scripts/*.py`. |
| Repo-level `aeonrift/tests/` | **STUB** | Every subdir (`determinism/golden/content/netcode/integration/e2e`) is README + `sample-*.ron` fixture only. The real tests live inside each game crate. |

**Build state (inferred, no heavy builds run):** each `apps/*` is an **independent Cargo workspace**
(there is **no root `Cargo.toml`** — mint is even excluded from the desktop workspace per its README).
mint/vend/abacus/turnstile/context-passport/game-economy/ledgerline/rit each carry their own
`Cargo.lock` and test suites the capability graph reports as passing. relay inherits full TensorZero
CI. Aeonrift keeps the two game **clients behind opt-in `client`/`netcode` features** so the default
`cargo build --workspace` stays green despite a Bevy 0.19 (games) vs 0.20-dev (engine fork) split.

---

## 3. Ship-blockers

### Half A — from mid-WIP to a shippable settlement rail
1. **Land the WIP branch.** ~220 uncommitted files across mint/abacus/vend/relay/turnstile/etc. hold
   the gap-closing work (catalogs, postgres stores, reapers, outbox). Nothing ships until this branch
   is reviewed, green, and merged. **Highest priority, codeable now.**
2. **Finish the mint-devserver integration harness** (the branch's namesake, commit `71e6873`). A
   headless end-to-end money-path harness (fund → hold → capture → receipt → reconcile) across mint +
   relay + abacus/vend is the single most valuable correctness artifact. **Codeable now.**
3. **Durability-under-failure tests for the money path.** Unit tests exist; what's missing is
   fault-injection proving: ledgerline outbox compensation nets to zero on mid-saga Mint failure;
   vend/abacus hold-reaper never releases a captured/fresh hold; turnstile nonce consume is exactly-once
   under concurrency. **Codeable now** (needs a Postgres in CI).
4. **Deploy story.** Only `vend` has a `Dockerfile`. Mint has migrations but no container/manifest;
   abacus/turnstile/ledgerline/game-economy have none. Need Dockerfiles + compose/k8s + a documented
   secret set (`DATABASE_URL`, `MINT_API_KEY`, `MINT_SIGNING_KEY`, payer-key registry, provider keys).
   **Codeable now; go-live blocked on** real provider credentials (Creem/DodoPay/Stripe/iyzico + webhook secrets).
5. **Contracts dedupe (composability).** Three hand-rolled Mint clients (vend/abacus/passport) and two
   AITP wire copies. The enterprise doc's `railyard-contracts` + `*-sdk` refactor (§Phase 0–2) is the
   "composable platform" unlock. **Codeable now**, but a coordinated cross-app refactor — do it after
   the branch lands, not during.
6. **Turnstile provider registration + settlement-status surface.** The money-path lease vocabulary and recovery are implemented; discovery/control-plane product work remains. **Codeable now.**
7. **Real payment credentials + KYC/compliance** for live funding. **Blocked on** Turkey-based MoR
   provider onboarding (Creem/DodoPay per portfolio memory) and legal/KYC posture. Mint already
   gates dev funding out of prod (commit `e0157b7`).

### Half B — to a playable / shippable Aeonrift
1. **Resolve the Bevy 0.19 ↔ 0.20-dev schism.** The real game clients are kept OUT of the default
   green build and are NOT integration-tested against the migrated engine. Until the games and engine
   agree on a Bevy version, "the game" isn't actually built as one artifact. **Codeable now** (version
   alignment / pinning), non-trivial.
2. **Pick ONE game to finish.** Ascendancy (cards) is the closer-to-shippable slice; MOBA is a thinner
   netcode spine. Recommend finishing **ascendancy** first.
3. **Backend/persistence/matchmaking.** Both games have only headless "server seams"
   (`server/matchmaking.rs` is a lobby stub). Accounts, persistence, real matchmaking, live-ops = missing.
4. **Wire art into an asset store.** Generated `.glb`/LoRA weights are gitignored and live outside git
   (content-addressed store per README) — not yet wired into the game's asset pipeline.
5. **Art pipeline scale + credentials.** Style-LoRA is done; the 3D/rig/toon-render chain is proven for
   **1 of 47** champions. **Blocked on** a valid `REPLICATE_API_TOKEN` + **GPU credits** (H100/Replicate),
   local Blender, and operator time to run the batch across the full roster. This is a single-operator
   batch toolchain, not an automated service.

---

## 4. Phased plan (two independent tracks)

### Track R — Settlement rail (primary product)

**R0 — Land the branch (codeable now).**
- Review + green the ~220-file WIP tree per workspace; merge `mint/devserver-integration-harness`.
- _Acceptance:_ every `apps/*` workspace `cargo test` green in CI; branch merged; README/enterprise-arch
  status badges updated to match reality (they currently understate completeness).

**R1 — Prove the money path end-to-end (codeable now; needs CI Postgres).**
- Complete the mint-devserver harness: fund → hold → capture → signed receipt → reconcile across
  mint + relay + abacus + vend, plus ledgerline margin projection.
- Add fault-injection: mid-saga Mint failure (ledgerline compensation nets to zero), reaper safety,
  concurrent nonce consume, hold clamp at ceiling.
- _Acceptance:_ one CI job spins Postgres, runs the harness + fault suite, asserts conservation
  (supply in = balances + reserve) and exactly-once receipts after induced crashes.

**R2 — Deployability (codeable now; go-live blocked on creds).**
- Dockerfiles for mint/abacus/turnstile/ledgerline/game-economy; compose + k8s manifests; documented
  secret matrix; startup secret-presence validation.
- _Acceptance:_ `docker compose up` brings the rail live against a throwaway Postgres; healthz green;
  a scripted checkout→spend→receipt round-trips on the dev ramp. Prod funding stays gated until a real
  MoR provider + webhook secret is configured.

**R3 — Composability refactor (codeable now; do after R0).**
- Build `railyard-contracts` (`railyard-money/ids/aitp/errors`, `eventbus-types`); collapse duplicated
  `Micros`/AITP types; retire the 3 hand-rolled Mint clients onto `mint-sdk`; add `relay-sdk`.
- _Acceptance:_ each app depends only on another app's `*-sdk` + `railyard-contracts`; no app pulls
  another's `*-core`/`*-store`.

**R4 — Hardening (mostly codeable now).**
- Turnstile provider registration + settlement status; mint control-plane (key rotation / KYC tiers);
  ledgerline explosion into rating/cost-feed/settlement + event bus (Postgres LISTEN/NOTIFY to start).
- _Acceptance:_ turnstile replica-safe incl. registration; margin events flow mint→relay→ledgerline.

### Track G — Aeonrift (secondary; game + art)

**G0 — Unify the build (codeable now).**
- Resolve the Bevy 0.19↔0.20-dev split so game clients build against the migrated engine in one green build.
- _Acceptance:_ `cargo build --workspace` with `client` features on compiles; ascendancy client runs.

**G1 — Ship ascendancy as the flagship slice (codeable now).**
- Finish the touch client screens, wire content, add account/persistence/matchmaking beyond the lobby stub.
- _Acceptance:_ a human can play a full card match on device; deterministic replay + `match_win` gates stay green.

**G2 — Fill the repo-level test harness (codeable now).**
- Turn `aeonrift/tests/{determinism,golden,e2e}` scaffolds into real cross-crate runners.
- _Acceptance:_ golden/determinism suites run in CI.

**G3 — Scale the art pipeline (BLOCKED on GPU + creds).**
- Run the LoRA-conditioned splash + Hunyuan3D + rig/toon-render chain across the full 47-champion
  roster; wire outputs into the content-addressed asset store.
- _Blocked on:_ `REPLICATE_API_TOKEN`, GPU/H100 credits, Blender, operator time. **Cannot proceed without these.**

**G4 — MOBA client (optional, later).** Only if ascendancy proves demand.

---

## 5. Honest scale note

- **The 770k LOC is not 770k of owned work.** ~535k is vendored TensorZero inside `apps/relay/`
  (~1589 of the TODO markers are upstream's). A large share of aeonrift's 85k is the migrated
  `lol_*`/`league_*` Bevy engine. **Owned, load-bearing code is roughly:** mint ≈23k, vend ≈15k,
  aeonrift games ≈10k, context-passport ≈4k, game-economy ≈3.5k, abacus ≈4k, turnstile/agent-runtime
  ≈2.5k each, ledgerline ≈2.6k, rit ≈5k. "Done fully" = making **this** shippable, not adding LOC.
- **LOC-heavy-but-incomplete:** `lol_champions` (17k) / `lol_core` (14k) are large but only reach the
  player through the not-yet-unified game clients (Bevy schism). Relay is 535k but the owner's value is
  3 small overlay crates. Big number ≠ done.
- **Dead / generated bloat (disk, not all git):**
  - `aeonrift/target/` = **25G** (gitignored build cache — fine, but reclaimable).
  - `slice/bevy_render/` = **6.7G** on disk (build artifacts); `slice/` tracks 158 turntable PNGs + a few scripts in git.
  - `.git` = **1.9G** (heavy history, likely image blobs).
  - `engine/{net,render,particle,vision,platform,ui-kit,economy,ai-bots,protocol}` = ~33-LOC stub crates
    whose real functionality already exists in the `lol_*` layer — un-reconciled seams, candidates for
    consolidation or deletion.
  - `apps/rithub/` = docs-only; `apps/ranguard/` = demo. Neither is on the shippable-rail path.
- **Doc drift is itself a risk.** README/enterprise-arch badges understate completeness, while mint's
  README points at design docs (`docs/mint/PLAN.md`) that don't exist in this repo. A "DONE" pass must
  re-sync docs to code.

---

## 6. Priority call (honest)

**Context (portfolio reality, from memory):** the whole portfolio has ≈0 users / ≈0 revenue; the
binding constraint is **distribution/demand, not build**. Finishing either half produces an asset, not
automatically a business. Weigh effort accordingly.

**Recommended ordering:**

1. **First: land Track R0 + R1 (money-path correctness).** This is the highest-value, lowest-cost,
   fully-codeable-now work. The settlement rail is the stated primary product, it is the half with
   **real financial-correctness risk**, and it is **~90% there** — the WIP branch already closed the
   gaps the docs still list as open. Merging + proving the money path end-to-end converts "mid-WIP" into
   "demonstrably correct core." Do this regardless of the user situation, because a half-merged money
   ledger is a liability.

2. **Second: R2 deployability** to the point of `docker compose up` on a dev ramp. Stop before wiring
   real payment credentials — that step is demand-gated and MoR-onboarding-blocked; don't pay for it
   until there's a payer.

3. **Defer R3/R4 (composability + control plane).** Real engineering, but it's "make the platform
   bigger/cleaner," not "make it shippable." With ≈0 users, polish here is premature.

4. **Aeonrift (Track G): finish only the free, codeable-now slice (G0–G2).** Unify the Bevy build and
   make **ascendancy** actually playable + tested — it's the most complete game surface and the closest
   to a demo-able artifact. **Do NOT spend GPU/Replicate budget on G3 (full-roster art) yet** — it's the
   most expensive, most external-dependency-heavy step, and produces art for a game with no players.
   The style-LoRA is already a finished, versioned deliverable; that's enough to demo the look.

5. **Blunt take on "worth finishing":** Given ≈0 demand, treat both halves as **portfolio/asset**
   work, not revenue work. The rail is worth getting to *correct + deployable* (small remaining cost,
   removes the danger of a half-finished ledger, and is the reusable substrate). Aeonrift is worth
   getting to *one playable tested slice* (cheap, demo-able) but **not** worth funding the full art/3D
   pipeline or the MOBA client until there is a signal that anyone wants to play it. Spend the marginal
   dollar on the rail's correctness and on **distribution experiments**, not on more LOC in either half.

---

_Inferences are marked "inferred" throughout. Build states were read from Cargo/CI/docs and code
inspection, not from running full builds. Nothing in the working tree was modified._
