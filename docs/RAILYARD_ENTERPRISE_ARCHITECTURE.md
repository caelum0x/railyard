# Railyard — Enterprise Architecture & Composability Plan

> **Type:** Planning document. Nothing here changes app code. It defines the
> *target* enterprise shape for each app and the interop model that makes them
> "usable in each other."
>
> **Baseline convention:** `apps/mint` (10 crates) is the gold standard. Every
> other app's target tree below is decomposed in mint's idiom:
> `*-core` (pure domain, no I/O) → `*-store` + `*-store-postgres` (ports +
> Postgres impl) → `*-protocol`/`*-contract` (wire types) → clients → services →
> `*-server` (binary) → `*-sdk` (thin public consumer crate) → `*-testkit`.
>
> **Scope:** mint, relay, agent-runtime, vend, abacus, turnstile, ledgerline.
> `context-passport` is covered in the composability model because it is an
> in-flight rail service (`passport-sdk`) that the graph references.

---

## 0. Design invariants (inherited by every app)

These are already true in the codebase and every target tree below preserves them:

1. **Each app is its OWN standalone cargo workspace**, excluded from the repo-root
   `Cargo.toml` so it never enters the `apps/desktop` GPUI/Metal build graph.
2. **Money math is integer microdollars** (ADR-0001). No floats in money paths.
3. **Success-only billing** (ADR-0002): failed work creates no Mint movement.
4. **Mint is the only vault.** Every other app holds *no* money; it calls Mint
   over HTTP for hold/capture/release/transfer and treats Mint receipts as the
   system of record. Local Postgres is metadata/projection only.
5. **All external I/O is behind injectable transport traits** (fake in tests,
   real reqwest in prod). This is what makes cross-app composition testable.
6. **sqlx runtime query API only** (no `query!` macros) so crates build with no
   live `DATABASE_URL`.

The composability plan is an extension of invariant #5: every app exposes a thin
`*-sdk`/`*-contract` crate that *is* the injectable seam other apps wire in.

---

## 1. Per-App: Current Structure, Maturity, Target Enterprise Tree

Legend for target trees: each line is `crate-name — one-line responsibility`.
Crates marked **(new)** do not exist yet; unmarked crates exist today.

---

### 1.1 mint — settlement ledger (THE convention baseline)

**Current structure (10 crates, ship-ready core):**

```
mint-core          — money (Micros), addresses (bech32), ids (ULID), blake3 hash, errors
mint-ledger        — double-entry ops: transfer, hold, allowance, ledger posting
mint-store         — storage ports: StoreTrait, balance/journal/clock + in-memory impl
mint-store-postgres— Postgres impl of the store ports (rows, tx, migrate, rebuild)
mint-receipts      — Ed25519 signed, hash-linked receipts, merkle log, verify, reserve proof
mint-reserve       — 1:1 reserve ledger + on-ramp reconciliation
mint-stream        — streaming ledger view
mint-protocol      — AITP-402 wire state machine: types/codec/intent/nonce/verifier/protocol
mint-payments      — on-ramp adapters (card, iap, crypto_tr) — ONLY crate allowed provider HTTP
mint-server        — axum binary: auth, router, handlers, dto, views, state
```

**Maturity:** Highest in the repo. Ten crates compile, workspace tests pass,
double-entry + reserve conservation + receipt hash-linking implemented. Gaps
(from its README): payer-key rotation is config-and-restart (no authenticated
control plane), KYC is a permissive dev gate, redeem/on-ramp depends on
configured providers.

**Target enterprise tree (add 4 crates → 14):**

```
mint-core            — (exists) domain kernel
mint-ledger          — (exists) double-entry engine
mint-store           — (exists) ports + memory impl
mint-store-postgres  — (exists) Postgres impl
mint-receipts        — (exists) signed receipt chain
mint-reserve         — (exists) reserve conservation
mint-stream          — (exists) stream view
mint-protocol        — (exists) AITP-402 state machine  ← promote to the shared contract
mint-payments        — (exists) on-ramp adapters
mint-server          — (exists) data-plane binary
mint-sdk         (new)— thin async client crate other apps depend on (accounts/holds/capture/
                        release/transfer/allowance) — replaces per-app hand-rolled mint clients
mint-controlplane(new)— authenticated payer-key rotation, KYC tiers, admin/operator surface
mint-events      (new)— publishes settlement/receipt events to the railyard event bus
mint-testkit     (new)— in-memory FakeMint + fixtures, so downstream apps test against one fake
```

**Exposes:** `mint-protocol` (AITP-402 wire contract, already re-exports the
shared vocabulary), `mint-sdk` (**new** — the client every other app should
consume), `mint-events` (settlement stream), signed receipts as system-of-record.

**Consumes:** nothing from siblings (mint is the substrate root). Only external
payment providers via `mint-payments`.

---

### 1.2 relay — model gateway / the model rail (TensorZero fork)

**Current structure (~40 crates, feature-rich but sprawling):**
`gateway` (aitp.rs, billing.rs, router.rs, routes/), `relay-billing`
(catalog/pricing/usage/ledger + mint/ + store/ — already Mint-backed via injected
transport), plus the vendored TensorZero crate family (`tensorzero-core`,
`-http`, `-client`, `-auth`, `-types`, `-inference-types`, `-optimizers`,
`-mcp`, `-node`, `-python`, `-otel`, …), `provider-proxy`, `relay-hedge`,
`autopilot-*`, `evaluations`, plus `clients/` (openai-go, openai-node, relay-js,
relay-ruby) and a Next.js `ui/`.

**Maturity:** Large, production-grade upstream (TensorZero) with Railyard's
billing seam bolted on (`relay-billing`, `gateway/aitp.rs`, `gateway/billing.rs`).
README-level gap: **per-model price catalog** — bills every model at one flat
default. The Railyard-specific surface (billing + AITP-402) is thin relative to
the vendored gateway.

**Target enterprise tree — the goal is a clean *Railyard overlay* on top of the
vendored TensorZero core, so the fork stays rebasable:**

```
# --- Railyard overlay crates (the part we own) ---
relay-billing        — (exists) Mint-backed usage→charge ledger (catalog/pricing/usage/ledger/mint)
relay-catalog    (new)— per-model price catalog (input/output µ$ per Mtoken) — closes the flat-rate gap
relay-aitp       (new)— extract gateway/aitp.rs into a crate: 402 challenge/quote issuance for inference
relay-cost-feed  (new)— publishes per-request provider COGS events (what ledgerline subscribes to)
relay-sdk        (new)— thin OpenAI-compatible + Railyard-billing client crate for sibling apps
relay-contract   (new)— stable request/response + cost/usage wire types (shared, no gateway deps)
gateway              — (exists) the binary: routing, provider fan-out, wires the overlay in
# --- Vendored TensorZero core (kept isolated for rebase; not restructured) ---
tensorzero-*         — (exists) upstream model/provider/inference engine, treated as a dependency
provider-proxy, relay-hedge, autopilot-*, evaluations, clients/, ui/  — (exist) unchanged
```

**Exposes:** `relay-sdk` (OpenAI-compatible inference + Railyard billing headers),
`relay-contract` (usage/cost wire types), `relay-cost-feed` (**new** — the COGS
event stream ledgerline reads), AITP-402 quotes via `relay-aitp`.

**Consumes:** `mint-sdk` (settlement — today via `relay-billing/mint/`),
`railyard-contracts` for shared event/money types.

---

### 1.3 agent-runtime — the shared payment substrate

**Current structure (3 crates, payment SDK ready — the linchpin of composability):**

```
aitp-client     — AITP-402 caller seam: amount/codec/error/gateway/mint/signer/stub/transport/types.
                  Ed25519 signing, StubPaymentGateway, reqwest MintPaymentGateway. NO rig/mint dep.
relay-provider  — RelayModel: a rig CompletionModel backed by Relay's OpenAI-compatible API
agent-core      — PaidCompletionModel decorator + build_agent/build_paid_agent helpers
```

**Maturity:** Small but strategically central. `aitp-client` is **already the
shared substrate** — abacus and turnstile both path-depend on it. It is
deliberately decoupled (no rig, no mint crate dep), which is exactly the SDK
shape the whole rail should copy.

**Target enterprise tree (add 4 crates → 7). The key move: promote `aitp-client`
to the canonical, independently-publishable payment SDK (`aitp-sdk`) so siblings
depend on the substrate, not on all of agent-runtime:**

```
aitp-client      — (exists) THE shared AITP-402 payment seam  ← canonical payment SDK for the rail
relay-provider   — (exists) rig model backed by Relay
agent-core       — (exists) paid-inference runtime/decorator
agent-tools  (new)— reusable rig Tool implementations (paid MCP tools, turnstile-priced tools)
agent-session(new)— multi-turn session/policy state (dynamic 402 quote consumption, budgets)
agent-server (new)— optional hosted agent-runtime binary (run paid agents as a service)
agent-testkit(new)— fakes for gateway/provider/tools so downstream agents test offline
```

**Exposes:** `aitp-client` — the payment SDK consumed by abacus + turnstile
(and the client half mirrored by mint's `mint-protocol` server half). Also
`relay-provider` for anyone building rig agents on the rail.

**Consumes:** Relay (via `relay-provider` → OpenAI-compatible API), Mint (via
`aitp-client`'s `MintPaymentGateway`), vendored `rig`.

---

### 1.4 vend — white-label metered inference gateway (2nd most mature)

**Current structure (9 crates + Python/TS SDKs; a real service):**

```
vend-core         — domain kernel: µ$ money, MarkupPolicy/SpendCap/FundingMode, ids, errors,
                    injectable MintTransport/RelayTransport traits + repo ports
vend-tenancy      — Tenant/App/EndUser hierarchy over TenantRepo port
vend-keys         — provisioning-key lifecycle (vk_live_/vk_test_, argon2id, verify, revoke, cap)
vend-store        — Neon Postgres impls of every repo port + migrations
vend-mint-client  — reqwest MintTransport against mint-server
vend-relay-client — reqwest RelayTransport against Relay
vend-settlement   — reserve→capture/release + durable idempotent 3-way payout split
vend-server       — axum data-plane + control/customer API + reconciler binaries
vend-testkit      — FakeMint/FakeRelay/InMemoryStore + fixtures
sdks/python, sdks/typescript — published client SDKs
```

**Maturity:** Very mature — full multi-tenant control plane, OAuth (GitHub/Google
PKCE), Argon2id sessions/RBAC, streaming with hold-then-capture, crash reconciler
(`vend-reconcile`), append-only audit, docker/k8s deploy, live smoke harness.
Gaps: BYOK/end-user funding modes not yet audited.

**Target enterprise tree (add 4 crates → 13):**

```
vend-core          — (exists) domain kernel + transport ports
vend-tenancy       — (exists) tenant/app/end-user
vend-keys          — (exists) provisioning-key lifecycle
vend-store         — (exists) Postgres ports
vend-mint-client   — (exists) → SHOULD become a thin wrapper over mint-sdk (dedupe)
vend-relay-client  — (exists) → SHOULD become a thin wrapper over relay-sdk (dedupe)
vend-settlement    — (exists) payout split engine
vend-server        — (exists) data plane + control plane binary
vend-testkit       — (exists) fakes
vend-billing   (new)— rating/markup catalog split out of core (per-model rate cards)
vend-events    (new)— publishes charge/settlement/payout events to the event bus
vend-sdk       (new)— unify Python+TS+Rust consumer SDK behind vend-contract wire types
vend-contract  (new)— stable control/data-plane wire types (OpenAPI-generated)
```

**Exposes:** `vend-sdk` / `sdks/*` (metered OpenAI-compatible inference for
resellers), `vend-contract`, `vend-events`.

**Consumes:** `mint-sdk` (settlement, reserve, payout legs), `relay-sdk`
(inference + `tensorzero_cost` for capture), `railyard-contracts`.

---

### 1.5 abacus — inline per-agent spend-budget proxy (kill-switch)

**Current structure (5 crates, single-process):**

```
abacus-core        — domain: decision/envelope/estimate/identity/ids/money/receipt + transport trait
abacus-mint-client — reqwest Mint client
abacus-upstream    — upstream provider forwarder
abacus-proxy       — axum binary: auth/config/enforce/handler/pricing/settle + local_store (JSONL)
abacus-testkit     — FakeMint/FakeUpstream/fixtures
```

**Maturity:** Compiles, tests pass, `abacus-proxy` runnable. Enforces hard,
receipted per-agent budgets via Mint holds→capture and kills a request the
moment it would breach. **Single-process limitation:** policy/receipt state is a
JSONL file store; multi-replica needs a shared transactional store. Already
path-depends on `aitp-client`.

**Target enterprise tree (add 6 crates → 11) — mirror mint's store split so it
becomes horizontally scalable:**

```
abacus-core          — (exists) budget/decision domain kernel
abacus-mint-client   — (exists) → rebase onto mint-sdk
abacus-upstream      — (exists) provider forwarder
abacus-proxy         — (exists) enforcement binary
abacus-testkit       — (exists) fakes
abacus-policy    (new)— budget/allowance policy engine split out of proxy/enforce.rs
abacus-store     (new)— storage PORTS for policy + receipt views (trait; today only JSONL)
abacus-store-postgres(new)— shared transactional store impl → closes the multi-replica gap
abacus-events    (new)— publishes budget-breach / kill-switch events to the bus
abacus-sdk       (new)— thin client + budget-policy types for embedders/dashboards
abacus-server    (new)— control-plane binary (manage agents/budgets) split from the data-plane proxy
```

**Exposes:** `abacus-sdk` (budget policy + enforced-proxy client), budget-breach
events. **Consumes:** `aitp-client` (payment seam — already), `mint-sdk`
(holds/capture/allowances/receipts), optionally `relay-sdk` for pre-flight cost
estimate.

---

### 1.6 turnstile — drop-in agent paywall ("Stripe for AI agents")

**Current structure (3 crates, engine solid / SDK thin):**

```
turnstile-core  — framework-agnostic AITP-402 paywall engine: challenge/verifier/settle/nonce/
                  guard/clock/config/error
turnstile-axum  — drop-in axum middleware + PaidRequest extractor + 402/receipt responses
turnstile-demo  — reference server (paid POST /v1/echo + open /healthz)
```

**Maturity:** Engine + Axum adapter are solid and reuse the real `aitp-client`.
Gaps: other framework adapters, MCP helper, registry/discovery, and published SDK
packages don't exist. Nonce store is in-memory → horizontal scale needs a shared
nonce store.

**Target enterprise tree (add 8 crates → 11) — grow the adapter/SDK surface and
add the shared nonce store, following mint's store split:**

```
turnstile-core        — (exists) paywall engine
turnstile-axum        — (exists) axum adapter
turnstile-demo        — (exists) reference server
turnstile-nonce-store(new)— nonce store PORT (extract from core) — trait seam
turnstile-nonce-postgres(new)— shared Postgres/Redis nonce store → closes the scale gap
turnstile-tower   (new)— generic tower::Service adapter (any tower/hyper stack)
turnstile-mcp     (new)— MCP tool paywall helper (price an MCP tool per-call)
turnstile-registry(new)— paid-endpoint discovery/registry + metadata control plane
turnstile-sdk     (new)— published Rust SDK; source of the TS/Python bindings
turnstile-contract(new)— stable 402 challenge/receipt wire types (re-exports mint-protocol)
turnstile-server  (new)— hosted reference verifier binary (productionized demo)
```

**Exposes:** `turnstile-sdk` + `turnstile-contract` (make any endpoint/MCP tool
paid-per-call), the paywall middleware family. **Consumes:** `aitp-client` (wire
types + `HttpTransport`, already), `mint-sdk`/`mint-protocol` (verify + capture
through mint-server — Turnstile forwards the agent's already-signed authorization,
never re-signs).

---

### 1.7 ledgerline — gross-margin lens (least mature)

**Current structure (single crate, Sx0 slice):**

```
src/domain.rs   — event + margin domain
src/store.rs    — Postgres event/projection store
src/mint.rs     — 3-call Mint settlement (hold revenue → capture → transfer COGS)
src/server.rs   — axum routes (/v1/events, /v1/margins)
src/main.rs, lib.rs, error.rs
migrations/0001_events.sql
```

**Maturity:** Lowest — a single-crate binary. Runnable Sx0: ingests usage events,
settles revenue + COGS through Mint, exposes request-level gross margin. Gaps
(README): request-time 3-call settlement is **non-atomic** — needs a durable
outbox/workflow; no rating catalog; does not yet subscribe to Relay's cost feed.

**Target enterprise tree (explode 1 crate → 9, in mint's idiom):**

```
ledgerline-core        (new)— margin domain: revenue/COGS/margin math, event types (from domain.rs)
ledgerline-store       (new)— storage ports for events + margin projections (trait)
ledgerline-store-postgres(new)— Postgres impl + migrations (from store.rs)
ledgerline-mint-client (new)— Mint settlement client → rebase onto mint-sdk (from mint.rs)
ledgerline-settlement  (new)— durable OUTBOX/workflow for hold→capture→COGS  ← closes the atomicity gap
ledgerline-rating      (new)— rating catalog: turn raw usage into revenue/COGS (Sx1)
ledgerline-cost-feed   (new)— subscriber to relay-cost-feed events (auto-COGS ingestion)
ledgerline-server      (new)— axum binary (from server.rs/main.rs)
ledgerline-sdk         (new)— thin client + margin query types for dashboards
```

**Exposes:** `ledgerline-sdk` (margin queries), margin projections.
**Consumes:** `mint-sdk` (revenue capture + COGS transfer — the "revenue" side),
`relay-cost-feed`/`relay-sdk` (provider COGS — the "cost" side),
`railyard-contracts` for event types. Ledgerline is a pure *reader/reconciler*:
it reads Relay's cost and Mint's revenue and never holds money.

---

## 1.8 Enterprise Scale Estimates — what "done" actually means

> **Why this section exists.** The failure mode is an agent shipping a handful of
> files and declaring an app "done." The yardstick below is **measured, not
> guessed**: mint is already 10 crates of real, tested code.
>
> **Measured yardstick (mint, today):** **10 crates · 108 Rust files · 23,025
> LOC** — and mint is only "ship-ready *core*" (it still lacks the control plane,
> KYC tiers, multi-currency, custody/anchoring). So even mint's real number is a
> *floor*, not the enterprise ceiling. Per-crate density from mint: ~10.8
> files/crate, ~2,300 LOC/crate; server/store crates are heavier (mint-server 22
> files, mint-store 4,350 LOC).
>
> Estimates below are **sizing targets** (deliberate, non-empty code + tests +
> migrations + deploy), calibrated to mint's density. They are ranges; the point
> is the order of magnitude, so nobody mistakes a thin slice for a finished
> product.

**Measured current state (Rust, excluding `target/`):**

| App | Crates | Files (now) | LOC (now) | Maturity |
|---|---:|---:|---:|---|
| mint | 10 | 108 | 23,025 | ship-ready core (still has gaps) |
| vend | 9 | 41 | 15,301 | very mature service |
| abacus | 5 | 26 | 3,988 | single-process slice |
| turnstile | 3 | 17 | 2,316 | engine + axum only |
| agent-runtime | 3 | 21 | 2,287 | payment SDK ready |
| ledgerline | 1 | 7 | 1,141 | Sx0 single-crate slice |
| relay (owned overlay) | few | — | ~subset of 893 rust files | fork + thin billing overlay |

Note: relay's workspace is **893 Rust files** total, but the vast majority is the
vendored TensorZero engine; the Railyard-*owned* overlay is a small subset today.

**Enterprise target sizing (owned code; calibrated to mint's ~2,300 LOC/crate):**

| App | Target crates | Est. files | Est. LOC | Multiple vs now |
|---|---:|---:|---:|---:|
| mint | 14 | 200–240 | 45,000–55,000 | ~2× current |
| relay (owned overlay) | +5 owned | 120–160 | 25,000–35,000 | large (excl. vendored core) |
| agent-runtime | 7 | 90–120 | 18,000–24,000 | ~8–10× |
| vend | 13 | 180–220 | 35,000–45,000 | ~2.5× |
| abacus | 11 | 150–190 | 28,000–36,000 | ~8× |
| turnstile | 11 | 150–200 | 26,000–34,000 | ~12× |
| ledgerline | 9 | 110–150 | 22,000–30,000 | ~20× |
| railyard-contracts | 5 | 50–70 | 8,000–12,000 | new |
| **Platform (owned Rust)** | **~75** | **≈1,050–1,350** | **≈207,000–271,000** | — |

**Plus non-Rust enterprise surface** (not in the Rust totals above): per-app
published SDK bindings (TS/Python/Go/Ruby — vend already ships 2, relay ships 4),
`migrations/`, `deploy/` (docker-compose + k8s + Prometheus), CI pipelines,
OpenAPI docs, e2e/smoke harnesses, and dashboards. Historically this is +30–40%
of file count. **Realistic owned platform grand total: ≈1,600–1,900 files,
≈300,000+ LOC**, on top of the vendored TensorZero engine (~800 files) inside
relay.

**Per-app "done" ladder (so a slice is never mistaken for the product):**

| App | "slice" (now) | "enterprise done" |
|---|---|---|
| mint | 108 files / 23K — core ledger works | 200+ files / ~50K — + control plane, KYC tiers, multi-currency, custody, sdk/events/testkit |
| relay | fork + billing overlay | +5 owned crates, per-model catalog, cost-feed, sdk/contract — flat-rate gap closed |
| agent-runtime | 21 files — 3 crates | 90+ files — tools, sessions, hosted server, testkit |
| vend | 41 files — data+control plane | 180+ files — billing catalog, events, unified sdk/contract, BYOK funding audited |
| abacus | 26 files — single-process JSONL | 150+ files — shared Postgres store (multi-replica), policy engine, control plane, events, sdk |
| turnstile | 17 files — engine + axum | 150+ files — shared nonce store, tower/mcp/registry adapters, published SDKs, hosted verifier |
| ledgerline | 7 files — 1 crate | 110+ files — 9-crate explosion, durable settlement outbox, rating catalog, cost-feed subscriber, sdk |

The number that matters: **a finished rail app is 100–240 files and 20K–55K LOC
of owned code — not "a couple files."** The current single-crate ledgerline (7
files) is ~6% of its enterprise target.

---

## 2. Composability Model — "used in each other"

### 2.1 The shared substrate (already true today)

- **`aitp-client`** (agent-runtime) is the payment SDK substrate. **abacus** and
  **turnstile** already path-depend on it. It has no rig/mint dependency by
  design — that decoupling is what lets any app embed it.
- **Everything settles on Mint.** relay-billing, vend-settlement,
  abacus-settle, turnstile-settle, and ledgerline-mint all call `mint-server`
  over HTTP for hold/capture/release/transfer and treat Mint receipts as the
  system of record. Mint's `mint-protocol` is the server-side mirror of
  `aitp-client`'s caller-side wire types (they serialize byte-for-byte).
- **Relay is the model rail** everything routes inference through
  (agent-runtime's `relay-provider`, vend's `vend-relay-client`, abacus's
  upstream option).
- **Ledgerline reads Relay's cost + Mint's revenue** to compute gross margin.

### 2.2 The mechanism (the headline)

Introduce a small number of **shared, independently-versioned crates** so an app
can depend on another app's *contract/SDK* without pulling its entire workspace:

**A. A shared contract workspace — `railyard-contracts/`** (new top-level workspace)

```
railyard-contracts/
  crates/
    railyard-money      — the ONE integer-µ$ money type + ADR-0001 rules (dedupe 6 copies of Micros)
    railyard-ids        — shared ULID/bech32 id newtypes and address format
    railyard-aitp       — the AITP-402 wire contract (challenge/authorization/receipt/codec),
                          the single source both mint-protocol and aitp-client re-export
    eventbus-types      — event envelopes: SettlementEvent, CostEvent, ChargeEvent, BudgetBreachEvent
    railyard-errors     — shared error envelope + API response format (success/data/error/meta)
```

Rule: `railyard-contracts` depends on **nothing** in Railyard. Every app depends
*down* into it. This kills the current duplication (each app re-declares its own
`Micros`, its own AITP types) and gives one wire vocabulary.

**B. Published thin SDK crates (one per producer app)**

Each app publishes a `*-sdk` (async client) and, where the wire surface is
richer than the client, a `*-contract` (types only). These are the **only**
crates a sibling may depend on:

| Producer | SDK crate | Contract crate | What it gives consumers |
|---|---|---|---|
| mint | `mint-sdk` | `mint-protocol`/`railyard-aitp` | accounts, hold/capture/release, transfer, allowance, receipts |
| relay | `relay-sdk` | `relay-contract` | OpenAI-compatible inference + usage/`tensorzero_cost` |
| agent-runtime | `aitp-client` | `railyard-aitp` | the AITP-402 caller seam (already the substrate) |
| vend | `vend-sdk` | `vend-contract` | metered resell inference + control plane |
| abacus | `abacus-sdk` | budget policy types | enforced-proxy client + budget policy |
| turnstile | `turnstile-sdk` | `turnstile-contract` | paywall any endpoint/MCP tool |
| ledgerline | `ledgerline-sdk` | margin query types | margin/COGS queries |
| context-passport | `passport-sdk` | passport wire types | resolve a locked passport → provider conditioning |

**Isolation rule (the owner's requirement):** an app depends on
`other-sdk` + `railyard-contracts` only — **never** on the producer's internal
`*-core`/`*-store`/`*-server` crates. The SDK crate has a *minimal* dependency
closure (serde + reqwest + railyard-contracts), so pulling `mint-sdk` into vend
does not drag mint's Postgres/receipt/reserve crates into vend's build graph.
This is exactly today's `aitp-client` shape generalized to every app.

**C. A common event bus — `eventbus-types` + a transport**

Producers emit typed events (`*-events` crates); consumers subscribe. Transport
is pluggable behind a trait (Postgres LISTEN/NOTIFY or NATS/Redis Streams for
scale). Primary flows:

- `mint-events` → SettlementEvent/ReceiptEvent → consumed by ledgerline, vend, abacus dashboards.
- `relay-cost-feed` → CostEvent (per-request provider COGS) → consumed by ledgerline.
- `abacus-events` → BudgetBreachEvent → consumed by ops/alerting.

**D. Stable service APIs** — each `*-server` keeps its HTTP contract stable and
OpenAPI-documented (vend already ships `/v1/control/openapi.json`); the `*-sdk`
crates are generated from / kept in lockstep with those OpenAPI docs.

### 2.3 Dependency / data-flow graph

```
                          ┌────────────────────────────────────────────┐
                          │            railyard-contracts/              │
                          │  railyard-money · railyard-ids ·           │
                          │  railyard-aitp · eventbus-types · errors   │
                          └───────────────▲────────────────────────────┘
                                          │ (every app depends DOWN into it)
   ┌──────────────────────────────────────┼───────────────────────────────────┐
   │                                       │                                    │
   │   PAYMENT SUBSTRATE                    │        MODEL RAIL                  │
   │  ┌─────────────────┐                   │     ┌─────────────────┐            │
   │  │ agent-runtime   │                   │     │      relay      │            │
   │  │  aitp-client ●  │───(caller seam)──►│     │  gateway +      │            │
   │  │  relay-provider │──────inference───────────► relay-billing  │            │
   │  │  agent-core     │                         │  relay-cost-feed│            │
   │  └───────┬─────────┘                         └───┬────────┬────┘            │
   │          │ aitp-client (path/sdk)                │ relay- │ cost events     │
   │          │                                        │  sdk   │                │
   │   ┌──────┴──────┐   ┌──────────────┐              │        ▼                │
   │   │  turnstile  │   │   abacus     │              │   ┌──────────┐          │
   │   │ (paywall)   │   │ (budget/kill)│              │   │ledgerline│◄─ mint   │
   │   └──────┬──────┘   └──────┬───────┘              │   │ (margin) │  events  │
   │          │ mint-sdk        │ mint-sdk             │   └────┬─────┘          │
   │          │ (verify+capture)│ (hold+capture)       │        │ mint-sdk       │
   │          ▼                 ▼                       ▼        ▼                │
   │        ┌───────────────────────────────────────────────────────┐          │
   │        │                        MINT                            │          │
   │        │  mint-server ◄─ mint-protocol (server mirror of aitp)  │          │
   │        │  ledger · receipts · reserve · store-postgres          │◄── vend  │
   │        │  → emits mint-events (SettlementEvent/ReceiptEvent)     │  vend-sdk│
   │        └───────────────────────────────────────────────────────┘          │
   │                              ▲                                              │
   │   ┌──────────────────────────┴─────────────┐                               │
   │   │  vend  (resell gateway)                 │  vend → relay-sdk (inference) │
   │   │  vend-server · vend-settlement          │  vend → mint-sdk (settle+split)│
   │   └─────────────────────────────────────────┘                              │
   └───────────────────────────────────────────────────────────────────────────┘

   context-passport (passport-sdk) ── resolve ──► relay (provider fan-out)
                                    ── settle ──► mint

   ●  = the shared substrate crate (aitp-client) already reused by turnstile+abacus
```

**Who calls whom (edges):**

| Consumer | Depends on (SDK) | For |
|---|---|---|
| agent-runtime | mint-sdk, relay-sdk | pay + inference |
| turnstile | aitp-client, mint-sdk | verify/capture the agent's payment |
| abacus | aitp-client, mint-sdk, (relay-sdk) | budget hold/capture + cost estimate |
| vend | mint-sdk, relay-sdk | metered inference + reserve/capture/payout split |
| ledgerline | mint-sdk, relay-cost-feed | revenue capture + provider COGS |
| context-passport | relay-sdk, mint-sdk | provider conditioning + settle |
| relay | mint-sdk | success-only billing of inference |
| **mint** | *(none)* | it is the substrate root |

---

## 2.5 Graph Engineering Layer — the platform IS a graph

The rail is not a set of services that happen to call each other; it is **one
shared entity graph** with money and inference flowing along typed, timestamped,
provenance-bearing edges. "Used in each other" = **the apps share one knowledge
graph and one task graph.** Two graphs matter: the **knowledge graph** (what the
platform *remembers* — entities and their relationships) and the **task graph**
(how the platform *works* — the money/inference execution DAG).

### 2.5.1 Platform Knowledge Graph (unified ontology)

**Competency questions the graph must answer** (the ontology's spec + test suite):

1. Which Agent authorized the Payment that settled a given Receipt?
2. For a Generation, what Provider served it, at what Cost, and what Revenue did it bill?
3. Which Holds are open against an Account right now (solvency)?
4. Did a PaidRequest's Nonce ever get reused (replay)?
5. Which Budget enforced (or killed) a request, and what Receipt proves it?
6. For a Tenant's Key, what Usage rolled up into which Payout legs?
7. What is the MarginEvent (Revenue − COGS) for a request, tracing to its Mint Receipt and Relay Cost?

**Core entity types (nodes)** — owned by the app that is their system-of-record:

| Entity | Owner app | One-line definition |
|---|---|---|
| `Account` | mint | a ledger identity (agent/tenant/provider/treasury), bech32 address |
| `Transfer` | mint | a completed double-entry movement between accounts |
| `Hold` | mint | a reserved amount pending capture/release (success-only) |
| `Receipt` | mint | Ed25519-signed, hash-linked proof of a settled operation (system-of-record) |
| `Allowance` | mint | a budget grant an agent can spend against |
| `Model` | relay | a routable inference model |
| `Provider` | relay | an upstream inference vendor |
| `Generation` | relay | one inference request/response |
| `Cost` | relay | provider COGS for a Generation (`tensorzero_cost`) |
| `Agent` | agent-runtime | an autonomous caller that runs paid inference |
| `PaymentAuthorization` | agent-runtime | an Ed25519-signed AITP-402 authorization over a Challenge |
| `Tenant`/`App`/`EndUser` | vend | the resell hierarchy |
| `Key` | vend | a provisioning key with caps/scope |
| `Usage` | vend | a metered, rated inference charge |
| `Payout` | vend | a provider/tenant/platform split leg |
| `Budget` | abacus | a hard per-agent spend policy |
| `Enforcement` | abacus | a decision (allow/kill) at request time |
| `PaidRequest` | turnstile | a 402-gated endpoint/MCP-tool call |
| `Nonce` | turnstile | replay-protection token for a Challenge |
| `MarginEvent` | ledgerline | Revenue − COGS for one request |
| `Passport` | context-passport | a locked character/brand identity resolved per provider |

**Core relation types (edges)** — precise verbs, each carrying `at` (time) +
`via`/`proof` (provenance, usually a Receipt id):

```
Agent            ─AUTHORIZES→        PaymentAuthorization   {at, signer_key}
PaymentAuthorization ─SETTLES_ON→    Mint(Account/Hold)     {at, nonce}
Hold             ─CAPTURED_AS→        Transfer               {at, amount_micros}
Transfer         ─PROVES→             Receipt                {at, hash_link}
Allowance        ─CONSTRAINS→         Agent                  {since, until}
Budget           ─ENFORCES→           Enforcement            {at, decision}
Enforcement      ─PRODUCES→           Receipt                {at}
Generation       ─SERVED_BY→          Provider               {at}
Generation       ─INCURS→             Cost                   {at, cost_micros}
Generation       ─BILLED_AS→          Usage                  {at, charge_micros}   (vend)
Usage            ─SPLIT_INTO→         Payout                 {at, leg}
PaidRequest      ─GUARDED_BY→         Nonce                  {at, expiry}
PaidRequest      ─CAPTURES→           Hold                   {at}                   (turnstile→mint)
Receipt          ─FEEDS→              MarginEvent            {at}   (revenue side)
Cost             ─FEEDS→              MarginEvent            {at}   (COGS side)
Passport         ─RESOLVED_FOR→       Provider               {at, version}
Passport         ─CONDITIONS→         Generation             {at}
```

**The end-to-end spine (one traversal, the "used in each other" the owner wants):**

```
Agent ─AUTHORIZES→ PaymentAuthorization ─SETTLES_ON→ Mint ─(Hold CAPTURED_AS Transfer)→
Transfer ─PROVES→ Receipt ─FEEDS→ MarginEvent ←FEEDS─ Cost ←INCURS─ Generation ─SERVED_BY→ Provider
```

**Representation choice** (per the modeling method): a **property graph** is the
default. The graph is *virtual and federated* — each app is authoritative for its
own node types in its Postgres, and cross-app edges are carried by the shared
`eventbus-types` envelopes (§2.2C) + stable ids from `railyard-ids`. `Receipt` is
the universal provenance anchor: nearly every cross-app edge references a Mint
Receipt id, which is exactly why Mint is the substrate root. Time + provenance are
first-class on every edge (validity interval or event timestamp + Receipt/source
id) — retrofitting provenance after the fact is impossible, so it is designed in.

### 2.5.2 Per-app local domain sub-graphs (and how they plug in)

Each app owns a local sub-graph; it plugs into the platform graph through the
shared contract crates (`railyard-contracts` ids/money/aitp + the app's `*-sdk`).

```
mint:          Account ─(HOLDS)→ Hold ─(CAPTURED_AS)→ Transfer ─(PROVES)→ Receipt
                  ▲                                                          │
               Allowance ─(CONSTRAINS)→ Account                             │ plug-in: mint-sdk +
                                                                            │ railyard-aitp; Receipt id is
                                                                            ▼ the platform provenance key
relay:         Model ─(ROUTES_TO)→ Provider ; Generation ─(INCURS)→ Cost
                  plug-in: relay-sdk (Generation/Usage) + relay-cost-feed (Cost events)
agent-runtime: Agent ─(AUTHORIZES)→ PaymentAuthorization ─(OVER)→ Challenge
                  plug-in: aitp-client — the caller-side wire types mint-protocol mirrors
vend:          Tenant ─(OWNS)→ App ─(ISSUES)→ Key ─(METERS)→ Usage ─(SPLIT_INTO)→ Payout
                  plug-in: mint-sdk (settle+split) + relay-sdk (Generation); emits vend-events
abacus:        Budget ─(ENFORCES)→ Enforcement ─(ON)→ Agent
                  plug-in: aitp-client + mint-sdk (Hold/Allowance/Receipt); emits BudgetBreachEvent
turnstile:     PaidRequest ─(GUARDED_BY)→ Nonce ; PaidRequest ─(CAPTURES)→ Hold
                  plug-in: aitp-client (verify) + mint-sdk (capture); Nonce store is the local scale-out
ledgerline:    MarginEvent ─(FROM)→ Receipt(revenue) + Cost(COGS)
                  plug-in: PURE CONSUMER — subscribes mint-events + relay-cost-feed; owns no money node
context-passport: Passport ─(CONDITIONS)→ Generation ; Passport ─(RESOLVED_FOR)→ Provider
                  plug-in: relay-sdk (fan-out) + mint-sdk (settle)
```

The composability rule restated in graph terms: **an app may create edges *to*
another app's nodes only through that app's `*-sdk`/contract, and may only
reference nodes by their `railyard-ids` id + Receipt provenance — never reach into
another app's store.**

### 2.5.3 Platform Task Graph (the money/inference execution DAG)

Nodes are jobs; an edge exists only when a job needs a prior job's *result*. This
is the per-request settlement DAG for a paid inference on the rail:

```
                         ┌──────────────── DIAMOND (parallel) ────────────────┐
                         │                                                    │
 [1] authenticate ──► [2] price/quote ──► [3] authorize-hold ──►  ┌─ [4a] forward-to-relay ─┐
      (Key/Agent)        (catalog +        (mint Hold,            │      (Generation)        │
                          402 challenge)    success-only)          ├─ [4b] budget-check ─────┤ ─► [5] VERIFY ─► [6] settle
                                                                   │   (abacus Enforcement)  │   (separate    (owned MERGE:
                                                                   └─ [4c] passport-resolve ─┘    context)     capture│release)
                                                                        (optional)                                │
                                                                                                                  ▼
                                                                              [7] meter-usage ──► [8] record-Receipt ──► [9] emit events
                                                                               (relay Cost)        (mint, signed)        (mint-events,
                                                                                                                          relay-cost-feed)
                                                                                                                          │
                                                                                                                          ▼
                                                                                                          [10] margin (ledgerline MarginEvent)
```

**Diamond pattern (§task-graphs):** jobs `[4a] forward-to-relay`,
`[4b] budget-check`, and `[4c] passport-resolve` are **independent workers** — they
don't read each other's output (fake "and then" edges deleted), so they run in
parallel after the hold. `[5] VERIFY` is a **separate verifier node** that owns
the money-safety decision (did usage arrive? is `tensorzero_cost` valid and within
the hold? did `[DONE]` land?) — this is exactly vend's stream-capture guard and
`vend-reconcile`: *the model/worker never grades its own capture.* `[6] settle` is
the **single owned merge**: capture-on-success or release-on-any-failure, one
writer, so uncoordinated failures can't double-capture.

**The stop rule applied:** the diamond is only worth it because those three jobs
genuinely split. The sequential spine (`authorize → verify → settle → receipt →
margin`) stays with one coordinator — each step needs the full picture, and
splitting it would amplify errors (per the DeepMind×MIT result). Ledgerline's
current **non-atomic 3-call settle** is the anti-pattern; `ledgerline-settlement`
(durable outbox) makes `[6]→[8]` one owned, crash-visible merge.

**Human gates (put them only where undo is expensive):**

| Gate | Where in the DAG | App |
|---|---|---|
| KYC / account-tier approval | before an Account can hold/redeem | mint-controlplane |
| Payer-key rotation approval | out-of-band, before `[3] authorize-hold` trusts a new key | mint-controlplane |
| Payout release approval | before `[6] settle` executes provider/tenant/platform legs above a threshold | vend-settlement |
| Budget policy change | before `[4b]` enforces a new Budget | abacus-server |
| Tenant/App provisioning | before a Key can meter | vend control plane |

Everything else (per-request hold/capture/release) is automated — a gate on every
request would make the human the bottleneck; gates sit only on the irreversible,
expensive-to-undo edges (money release, identity/KYC, key trust).

**Guardrails (the four caps):** every reconcile/retry loop has a max round count
(vend's lease-bounded reconciler); **one writer per money artifact** (the owned
`[6] settle` merge; Mint idempotency keys); routing lives in written steps
(deterministic operation keys, not model-chosen); and settlement fan-out is
bounded (the fixed 3-way payout split, not unbounded).

---

## 3. Build / Refactor Order

Ordered so each step unblocks the next and nothing is done before its dependency.

**Phase 0 — Contracts first (unblocks everything).**
1. Create `railyard-contracts/` workspace: `railyard-money`, `railyard-ids`,
   `railyard-aitp`, `eventbus-types`, `railyard-errors`.
2. Make `mint-protocol` and `aitp-client` re-export `railyard-aitp` (keep their
   public APIs identical — this is a source-compatible move).

**Phase 1 — Publish the substrate SDKs (mint + relay + aitp).**
3. Extract `mint-sdk` from mint (thin client; minimal deps).
4. Add `mint-testkit` (FakeMint) and `mint-events`.
5. Extract `relay-sdk` + `relay-contract`; add `relay-catalog` (closes the
   flat-rate gap) and `relay-cost-feed`; extract `relay-aitp`.
6. Promote `aitp-client` as the canonical payment SDK; add `agent-testkit`.

**Phase 2 — Rebase consumers onto the SDKs (dedupe hand-rolled clients).**
7. vend: point `vend-mint-client`/`vend-relay-client` at `mint-sdk`/`relay-sdk`;
   add `vend-sdk`/`vend-contract`/`vend-events`.
8. abacus: rebase `abacus-mint-client` on `mint-sdk`; add `abacus-store` +
   `abacus-store-postgres` (**scale unblocker**), `abacus-policy`, `abacus-sdk`.
9. turnstile: add `turnstile-nonce-store` + `turnstile-nonce-postgres` (**scale
   unblocker**), `turnstile-sdk`/`turnstile-contract`, `turnstile-mcp`.

**Phase 3 — Ledgerline explosion + event wiring (last, depends on all feeds).**
10. Explode ledgerline into its 9-crate tree; add `ledgerline-settlement`
    (durable outbox — **atomicity unblocker**), `ledgerline-rating`,
    `ledgerline-cost-feed`, `ledgerline-sdk`.
11. Wire the event bus end-to-end: mint-events + relay-cost-feed → ledgerline.

**Phase 4 — Hardening.**
12. mint-controlplane (authenticated key rotation / KYC tiers).
13. Generate `*-sdk` bindings (TS/Python) from each `*-server` OpenAPI doc;
    consolidate vend's existing `sdks/python`+`sdks/typescript` under `vend-sdk`.

---

## 4. Key Decisions for the Owner

1. **One shared contracts workspace vs. per-app duplication.** Recommendation:
   build `railyard-contracts` and collapse the ~6 duplicated `Micros`/money
   types and the two AITP wire copies (mint-protocol vs aitp-client) into it.
   This is the single highest-leverage move for "composable" and it makes every
   later SDK trivial. Cost: a coordinated refactor across all apps.

2. **Thin `*-sdk` per app is the composition primitive.** Enforce the rule that
   an app may depend only on another app's `*-sdk` + `railyard-contracts`, never
   its `*-core`/`*-store`/`*-server`. `aitp-client` already proves the pattern
   (turnstile + abacus consume it with a tiny dep closure). This is what keeps
   an app from "pulling another's whole workspace."

3. **mint-sdk retires three hand-rolled Mint clients.** vend-mint-client,
   abacus-mint-client, and ledgerline's `mint.rs` are three copies of the same
   client. Consolidate to `mint-sdk`. Same for `relay-sdk` (relay-provider +
   vend-relay-client).

4. **The three known scale/atomicity gaps map cleanly to new crates.** Shared
   store for abacus (`abacus-store-postgres`), shared nonce store for turnstile
   (`turnstile-nonce-postgres`), durable outbox for ledgerline
   (`ledgerline-settlement`). All three follow mint's existing store/port split.

5. **Relay stays a rebasable overlay.** Keep the vendored TensorZero crates
   untouched and put all Railyard value (billing, catalog, aitp, cost-feed, sdk)
   in owned overlay crates. Do not restructure the fork.

6. **Event bus transport is pluggable and can start simple.** Postgres
   LISTEN/NOTIFY is enough for Sx0; the `eventbus-types` + trait seam lets you
   swap in NATS/Redis Streams later without touching producers/consumers.

7. **Target crate counts** (current → target): mint 10→14, relay overlay
   +5 owned crates, agent-runtime 3→7, vend 9→13, abacus 5→11, turnstile 3→11,
   ledgerline 1→9, plus the new 5-crate `railyard-contracts` workspace.

8. **Model the rail as one graph, not seven services.** The composability the
   owner wants IS the shared knowledge graph (§2.5): every app owns its node
   types, and cross-app edges are carried by `eventbus-types` + `railyard-ids`
   with a Mint `Receipt` as the universal provenance anchor. Adopt the property-
   graph federation model and design time+provenance onto every edge now —
   retrofitting provenance after fusion is impossible. The per-request settlement
   **task graph** (§2.5.3) fixes the diamond (parallel forward/budget/passport →
   separate verifier → one owned capture-or-release merge) and puts human gates
   only on irreversible edges (KYC, key trust, payout release).

9. **Scale reality (§1.8): "done" is 100–240 files / 20–55K LOC per app, not a
   handful.** Measured mint is already 108 files / 23K LOC and still incomplete.
   The owned platform target is ≈1,600–1,900 files and ≈300K+ LOC (plus the
   vendored TensorZero engine). Ledgerline at 7 files is ~6% of its target. Track
   each app against its per-phase "done" ladder so a thin slice is never signed
   off as the enterprise product.

10. **Doc-vs-code drift to fix.** Several READMEs (abacus, turnstile, ledgerline)
   link to `docs/apps-vision/*.md`, which **do not exist** in the repo. Either
   add those vision docs or correct the links; this plan is grounded in the
   actual code, not those missing docs.

---

## 5. Target Crate-Count Summary

| App | Current | Target | Net new | Key new crates (why) |
|---|---:|---:|---:|---|
| mint | 10 | 14 | +4 | mint-sdk, mint-controlplane, mint-events, mint-testkit |
| relay | ~40 (fork) | +5 owned | +5 | relay-catalog, relay-aitp, relay-cost-feed, relay-sdk, relay-contract |
| agent-runtime | 3 | 7 | +4 | agent-tools, agent-session, agent-server, agent-testkit |
| vend | 9 | 13 | +4 | vend-billing, vend-events, vend-sdk, vend-contract |
| abacus | 5 | 11 | +6 | abacus-policy, abacus-store(+postgres), abacus-events, abacus-sdk, abacus-server |
| turnstile | 3 | 11 | +8 | nonce-store(+postgres), tower, mcp, registry, sdk, contract, server |
| ledgerline | 1 | 9 | +8 | full mint-idiom explosion + settlement outbox + rating + cost-feed + sdk |
| **railyard-contracts** | 0 | 5 | +5 | money, ids, aitp, eventbus-types, errors (shared substrate) |

The composability mechanism in one line: **one shared `railyard-contracts`
workspace at the bottom, one thin `*-sdk`/`*-contract` per app in the middle, a
typed event bus on the side — and the hard rule that apps compose only through
those, never through each other's internals.**
