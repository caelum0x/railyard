# Railyard OSS Foundations — Fork / Build-On Map

> **Purpose.** Every Railyard app + Passport targets 20K–55K LOC of *owned* code
> (see `RAILYARD_ENTERPRISE_ARCHITECTURE.md` §1.8: the owned platform is
> ≈300K+ LOC). This document is the anti-"write it all from scratch" plan: for
> each app it names the **MIT/Apache-2.0 (or otherwise permissive)** open-source
> foundation to fork or reference, so we build on proven code.
>
> **Read-only research artifact.** It changes no app code.
>
> **Two reuse sources:**
> - **(A) Internal** — `apps/relay/crates` (already **Apache-2.0**, vendored
>   TensorZero + Railyard-custom crates) that are reusable across the platform.
> - **(B) External** — permissively-licensed OSS to fork/adopt per app.
>
> **License discipline.** Targets are verified MIT or Apache-2.0 wherever
> possible. **GPL/AGPL and non-commercial model weights are flagged as
> avoid-or-risk.** Where a widely-used dependency uses another permissive license
> (BSD-3, ISC, MPL-2.0, unique ring license), it is called out explicitly rather
> than glossed as "MIT/Apache."

---

## 0. Summary table — one top foundation per app

| App | Top recommended foundation | License | Fork or reference | Headline |
|---|---|---|---|---|
| **rit** (git core) | **gitoxide** (`gix` crate family) | Apache-2.0 **OR** MIT | Fork/vendor specific `gix-*` crates | Building on `gix` saves **~100K+ LOC** vs. a from-scratch Git |
| **rithub** (forge) | **Gitea** (model + selective port) | **MIT** | Reference architecture; port modules, not a Go fork | Forgejo is **GPLv3 — avoid**; reuse Gitea's *shape*, keep Rust-on-`rit-sdk` |
| **mint** (ledger) | **TigerBeetle** (accounting model) + `rust_decimal` | Apache-2.0 / MIT | Reference (TB) + adopt (`rust_decimal` where non-integer) | Mint already integer-µ$; TB is the *reference* for two-phase transfers |
| **relay** (model rail) | **TensorZero** (already vendored) | **Apache-2.0** | Fork + thin overlay (the model to copy) | The proven "fork + rebasable overlay" pattern for the whole rail |
| **agent-runtime** | **rig** (already vendored) | **MIT** | Adopt as dependency | `CompletionModel`/`Agent` traits already power `relay-provider` |
| **vend / abacus / turnstile / ledgerline** | **axum + tower + sqlx + tokio** stack | MIT / (sqlx MIT OR Apache-2.0) | Adopt (already in use) | The standard permissive service stack; no billing OSS worth forking |
| **ranguard** (attestation) | RustCrypto (`sha2`/`hmac`/`ed25519-dalek`) + `rustls`/`ring` + `c2pa-rs` | MIT/Apache-2.0 (dalek **BSD-3**, ring **unique**) | Adopt crates; reference TPM/DCAP libs | Replaces demo hand-rolled crypto; C2PA for signed evidence |
| **Passport** | **c2pa-rs** (provenance) + **`ort`** ONNX (consistency) + internal `bip39-uuid-substitution` (ids) | Apache-2.0 OR MIT | Adopt c2pa-rs/ort; **model weights are the license risk** | The moat (`passport-consistency`) rides `ort`; **insightface weights are non-commercial — flag** |

**Biggest wins (ranked):**
1. **rit → gitoxide**: a complete Rust Git is ~106K LOC of owned code (per
   `rit/ARCHITECTURE.md` §7). `gix` already implements object model, packfiles +
   delta, refs, index, worktree, protocol v2, and diff. Reusing it saves the
   overwhelming majority — call it **~100K LOC** and years of correctness work.
2. **relay → TensorZero (done)**: the ~800-file vendored engine is the single
   largest reuse on the platform; Railyard owns only a thin billing/AITP overlay.
3. **rithub → Gitea-as-reference**: a forge is 250K–450K LOC; you cannot fork Go
   into Rust, but Gitea's domain model (RBAC, PR state machine, webhooks, outbox)
   is the proven blueprint that de-risks the largest app.
4. **Passport → c2pa-rs + ort**: provenance signing (C2PA) and cross-provider
   similarity scoring are two heavy subsystems available as permissive Rust — the
   moat does not need to be built from zero.
5. **Internal `bip39-uuid-substitution`, `durable-tools*`, `ts-executor-pool`,
   `reqwest-sse-stream`, `minijinja-utils`, `googletest-matchers`** already exist
   Apache-2.0 and should be consumed platform-wide instead of re-implemented.

---

## Part A — Internal reuse map (`apps/relay/crates`, Apache-2.0)

The relay workspace is **Apache-2.0** (`apps/relay/Cargo.toml`:
`license = "Apache-2.0"`, `apps/relay/LICENSE`). All 36 crates inherit it via
`license.workspace = true`. These are *already ours* — the cheapest reuse on the
platform. Split into **Railyard-custom** (portable, high value) and **vendored
TensorZero** (the relay engine).

### A.1 Railyard-custom crates — reuse across apps

| Crate | What it does | Which apps should consume it |
|---|---|---|
| **`bip39-uuid-substitution`** | Bidirectional UUID ↔ human-readable BIP39 word-triples (`apple-banana-cherry`); deterministic via blake3 of the UUID's low 8 bytes (tuned for UUIDv7). Built to stop LLMs mangling hex ids. | **Passport** (human-readable `PassportId`/`VersionId` in prompts + UIs), **mint** (memorable account/receipt refs), **abacus/turnstile** (agent/budget ids in 402 challenges), **rithub** (short repo/PR handles). Already re-exported by `ts-executor-pool` as `uuid_substitution`. |
| **`durable-tools`** | Durable tool-execution framework (tool registration, workers, inference) over the `durable` crate — crash-safe agent tool runs. | **agent-runtime** (`agent-tools`/`agent-session` target crates), **rithub** (`forge-jobs` durable outbox handlers), **ledgerline** (`ledgerline-settlement` durable outbox), **mint** on-ramp workflows. |
| **`durable-tools-spawn`** | Lightweight "spawn a durable task by name" — no `tensorzero`/`schemars` weight. Just `SpawnClient` + `TaskToolParams` over a DB queue. | Anywhere a small durable-job enqueue is needed without the full framework: **vend** reconciler, **abacus** control-plane jobs, **ledgerline** outbox producer. |
| **`ts-executor-pool`** | Secure JS/ECMAScript executor pool using **SES (Secure ECMAScript)** — `lockdown`/`harden`/`Compartment` sandboxing, TS type-checking, LLM-query runtime, tool-result plumbing. | **turnstile** (sandboxed paid-tool/MCP execution), **agent-runtime** (untrusted tool code), **rithub** (`forge-ci` sandboxed hook scripts), **Passport** (`passport-personalize` hook evaluation). |
| **`provider-proxy`** | Caching MITM HTTP/HTTPS proxy that records/replays upstream provider responses to disk (based on MIT `http-mitm-proxy`, openssl removed). Test infra for flaky provider calls. | **vend**, **abacus**, **relay**, **Passport** — deterministic offline tests of provider fan-out (`*-testkit` crates). Test-only dependency. |
| **`reqwest-sse-stream`** | `reqwest` → typed SSE `EventStream` with careful error-source chaining. The streaming primitive under gateway token streams. | **vend** (streamed metered inference), **abacus** (stream-then-capture), **Passport** (`passport-relay-client` streaming conditioning), any `*-relay-client`. |
| **`minijinja-utils`** | Static analysis of MiniJinja templates — extracts `include`/`import`/`from`/`extends` deps by walking the AST; decides static resolvability. | **rithub** (`forge-markup`/notification templates), **Passport** (`passport-personalize` prompt templates), **relay** prompt-template validation. Pair with upstream `minijinja` (Apache-2.0). |
| **`googletest-matchers`** | Custom `googletest` matchers for `serde_json::Value` (`matches_json!`). Test ergonomics for JSON APIs. | **Every app's `*-testkit`** — mint, vend, abacus, turnstile, Passport, rithub API tests. Platform-wide test dependency. |
| **`config-applier`** | Targeted, format-preserving edits to TOML config (upsert variant/experimentation/evaluation/evaluator) with a locator + path resolver. | **relay** control plane; generalizable to any app with an operator-editable TOML config surface (**vend**, **abacus** policy config). |
| **`relay-hedge`** | Additive hedged dispatch (race N staggered attempts, first-token wins, cancel losers at the socket) + EWMA per-endpoint SLO/health routing. | **relay** (its home); the health/hedge pattern is reference for **vend**/**abacus** upstream selection and **Passport** provider fan-out (`generate-AS` diamond, §5 of Passport arch). |
| **`relay-billing`** | Mint-backed usage→charge ledger (catalog/pricing/usage/ledger + injected Mint/Relay transport). The Railyard billing seam on TensorZero. | The template for **vend-billing**, **abacus** pricing, **ledgerline-rating** — copy the injected-transport pattern rather than re-inventing it. |
| **`autopilot-*`** (`-client`, `-tools`, `-worker`) | Autopilot agent loop (client, tool defs, durable worker) over `durable-tools`. | **agent-runtime** target `agent-server`/`agent-session`; reference for **rithub** CI-runner agents. |
| **`evaluations`** | TensorZero eval harness (datasets, evaluators, scoring). | **Passport** `passport-consistency` eval harness (the moat's offline gate); **relay** model eval. Direct conceptual reuse. |
| **`tensorzero-auth`** | Auth layer (sqlx-backed) for the gateway. | Reference for `*-server` API-key/session patterns across apps. |

### A.2 Vendored TensorZero family — the relay engine (keep isolated, rebasable)

All Apache-2.0. Treated as an upstream dependency behind the Railyard overlay
(per `RAILYARD_ENTERPRISE_ARCHITECTURE.md` §1.2 "keep the fork rebasable").

| Crate | Role | Cross-app reuse |
|---|---|---|
| `tensorzero-core` | Model/provider/inference engine, routing, config. | relay only (engine). |
| `tensorzero-http` | HTTP surface for the gateway. | relay; patterns reusable. |
| `tensorzero-client` / `-node` / `-python` / `-ts-types` | Language clients + typings. | Model for **vend**/**Passport** SDK generation. |
| `tensorzero-inference-types` / `-types` / `-types-providers` / `-inference-types` | Shared inference/provider wire types. | Source of `relay-contract`; **Passport**/**vend** map onto these. |
| `tensorzero-mcp` | MCP server (`rmcp`, streamable HTTP). | **turnstile-mcp** paid-tool paywall; **agent-runtime** MCP tools. |
| `tensorzero-optimizers` | Prompt/variant optimization. | relay; reference for Passport seed-bank tuning. |
| `tensorzero-otel` | OpenTelemetry wiring. | **Every `*-server`** — platform-wide observability. |
| `tensorzero-derive` / `-error` / `-config-paths` / `-stored-config` / `-otel` / `-overhead` / `-unsafe-helpers` | Supporting crates (derive macros, error taxonomy, config, benchmarking, minimal unsafe helpers). | Internal; `-error` and `-otel` conventions worth mirroring platform-wide. |

**Rule (inherited):** sibling apps depend only on relay's thin `*-sdk`/`*-contract`
overlay crates, **never** on `tensorzero-core` internals — same isolation rule as
the rest of the rail (§2.2 of the enterprise arch).

---

## Part B — External OSS foundations, per app

Each table: **foundation → license → fork or reference → reuse vs build →
integration note.** Licenses reflect the projects' stated licensing; **verify at
adoption time** (pinned commit), especially model *weights*, which are frequently
licensed separately and more restrictively than the code.

### B.1 rit — Git core

The single biggest external-reuse opportunity on the platform.

| Foundation | License | Fork / reference | Reuse vs build | Integration note |
|---|---|---|---|---|
| **gitoxide (`gix`, `gix-object`, `gix-pack`, `gix-odb`, `gix-ref`, `gix-index`, `gix-worktree`, `gix-diff`, `gix-protocol`, `gix-transport`, `gix-hash`, …)** | **Apache-2.0 OR MIT** | **Fork/vendor selected crates** | Reuse: object model, loose+packed ODB, **pack/delta**, refs+packed-refs, index v2/3/4, worktree, diff, **protocol v2** (upload/receive-pack), transport (HTTP/SSH). Build: the Railyard `rit-sdk` facade, the compat-gate testkit/fuzz harness, any SHA-256 gaps `gix` hasn't finished. | Map `rit`'s target crates (`rit-object`/`-odb`/`-pack`/`-refs`/`-index`/`-worktree`/`-protocol`/`-diff`, per `rit/ARCHITECTURE.md` §2) onto the matching `gix-*` crates. Either depend on `gix-*` directly behind `rit-sdk`, or vendor+adapt the crates whose API you want to own. Keeps the **hash-agnostic core** invariant — `gix-hash` is already generic over SHA-1/SHA-256. **This is the ~100K LOC win.** |
| **`git2` (libgit2 Rust bindings)** | Bindings **MIT OR Apache-2.0**; **libgit2 itself is GPLv2 *WITH* the GNU linking exception** | Reference / fallback only | Reuse: mature, battle-tested Git ops if `gix` has a gap. | **License caveat:** libgit2's GPLv2-with-linking-exception is *not* MIT/Apache. The linking exception permits linking from non-GPL apps, but it is a heavier obligation than `gix`'s dual permissive license. **Prefer `gix`**; treat `git2` as a differential-test oracle or last-resort fallback, not the foundation. |
| **Reference `git` CLI (2.50+)** | GPLv2 (tool, not linked) | Reference (differential testing only) | The interop oracle `rit`'s testkit already shells out to (`tests/interop.rs`, 35 tests). | Not a code dependency — shelling out to a GPL *tool* creates no license obligation. Keep it as the compat gate. |

**Recommendation:** Build `rit` **on gitoxide**, not from scratch. Even taking the
lower-level `gix-*` crates (object/pack/odb/ref/index) and writing your own
higher-level porcelain + `rit-sdk` captures the bulk of the ~106K-LOC estimate
while letting you own the public API. Preserve the compat-gate discipline
(differential tests, fuzzing, quarantine) — that is Railyard-owned regardless.

### B.2 rithub — collaboration forge

You cannot fork a Go/Ruby forge into the Rust-on-`rit-sdk` design. Reuse the
**domain model and architecture**, port selectively.

| Foundation | License | Fork / reference | Reuse vs build | Integration note |
|---|---|---|---|---|
| **Gitea** | **MIT** | **Reference architecture** (port modules, don't fork the Go) | Reuse (as blueprint): RBAC model, PR state machine, issues/labels/milestones schema, webhook + delivery model, protected-branch rules, notification fan-out, migrations discipline. Build: everything in Rust over `rit-sdk` + Postgres per `rithub/ARCHITECTURE.md`. | Gitea's MIT license means you *may* copy small pieces of code/SQL, but the real value is the proven **shape** for `forge-domain`/`forge-rbac`/`forge-store`/`forge-webhooks`. Gitea is the closest single-node analog to rithub's target. |
| **Forgejo** | **GPLv3** | **Avoid for code reuse** | Reference at arm's length only. | ⚠️ Forgejo relicensed to **GPLv3** — copying its code would impose copyleft on rithub. Read it for ideas, do not lift code. Prefer Gitea (MIT) as the reference. |
| **GitLab CE** | **MIT** (Community Edition) | Reference only (Ruby/Vue, millions of LOC) | Reference for large-scale feature completeness (CI, review). | Too large to port; use as a feature-completeness checklist, not a code source. GitLab EE is proprietary — read only CE. |
| **Tantivy** | **MIT** | **Adopt** | Reuse: full-text search for `forge-search` (code/issue/PR indexing). Build: incremental index jobs fed by the outbox. | Rust-native, already named in `rithub/ARCHITECTURE.md` §2. Drop-in for the search crate. |
| **`comrak` / `pulldown-cmark` + `ammonia`** | comrak **BSD-2**, pulldown-cmark **MIT**, ammonia **MIT/Apache-2.0** | **Adopt** | Reuse: CommonMark rendering (`comrak`) + HTML sanitization (`ammonia`) for `forge-markup`. Build: @/#-ref linking, syntax highlight (`syntect`, MIT). | Sanitized Markdown is an in-scope threat (`rithub` §1); use audited crates, do not hand-roll. comrak's BSD-2 is permissive (note: not literally MIT/Apache). |
| **axum + tower + sqlx + `russh`** | axum/tower **MIT**, sqlx **MIT OR Apache-2.0**, **`russh` Apache-2.0** | **Adopt** | Reuse: HTTP service (`forge-http`), SSH gateway (`forge-ssh` via `russh`), Postgres store. | `russh` is the pure-Rust SSH server for the `git-upload/receive-pack` SSH channel. Standard stack. |

**Recommendation:** rithub = **Gitea's blueprint, rebuilt in Rust on `rit-sdk`**,
with Tantivy/comrak/ammonia/axum/russh adopted directly. The one hard code
dependency is `forge-git` → `rit-sdk` (i.e., → gitoxide underneath).

### B.3 mint — settlement ledger

Mint is already the most mature app (10 crates, integer-µ$, Ed25519 receipts).
External OSS here is mostly **reference**, not fork — money correctness is owned.

| Foundation | License | Fork / reference | Reuse vs build | Integration note |
|---|---|---|---|---|
| **TigerBeetle** | **Apache-2.0** | **Reference** (concepts, not code — it's Zig) | Reuse (as model): two-phase transfers (pending→post/void) = exactly Mint's hold→capture/release; debit/credit invariants; reserve conservation; idempotency-by-id. Build: Mint's own Rust engine. | TigerBeetle is the canonical modern double-entry accounting reference. Mint's `hold/capture/release` already mirrors TB's two-phase transfer. Read TB's data model + safety docs to validate `mint-ledger`/`mint-reserve` invariants. Do **not** adopt TB as the store (Zig, different deployment) unless you want it as an external ledger engine later. |
| **`rust_decimal`** | **MIT** | **Adopt (selectively)** | Reuse: exact base-10 decimal for any *display/FX/rating* math that isn't integer-µ$. Build: keep the money path integer-µ$ (ADR-0001). | Money movement stays integer microdollars (no floats, per invariant). `rust_decimal` is for rate cards / FX / rounding at the edges (relay-catalog, vend-billing, ledgerline-rating), not the ledger core. |
| **RustCrypto `ed25519-dalek` + `blake3` + `sha2`** | ed25519-dalek **BSD-3-Clause**; blake3 **CC0/Apache-2.0/dual**; sha2 **MIT OR Apache-2.0** | **Adopt** (already in use) | Reuse: signed hash-linked receipts (`mint-receipts`), Merkle log. | ⚠️ `ed25519-dalek` is **BSD-3-Clause** (permissive, but not MIT/Apache) — fine to use, just record it accurately in `deny.toml`. Already the receipt-signing basis. |
| **`bech32`** | **MIT** | **Adopt** | Reuse: Mint account addresses (already used). | Standard address encoding. |
| Ledger references (Beancount, Ledger-cli, hledger) | Beancount **GPLv2**, Ledger **BSD**, hledger **GPLv3** | **Reference only** — do not link | Concept reference for double-entry ergonomics. | ⚠️ Beancount/hledger are GPL — do not copy code. Ledger (BSD) is safer to read. All are plaintext-accounting tools, not embeddable engines; reference the *model* only. **Firefly III is AGPL — avoid entirely.** |

**Recommendation:** Mint stays owned; **TigerBeetle is the reference** that
validates the two-phase settlement model, and `rust_decimal` covers non-integer
edge math. No fork.

### B.4 relay — model rail (the "fork + thin overlay" model)

| Foundation | License | Fork / reference | Reuse vs build | Integration note |
|---|---|---|---|---|
| **TensorZero** | **Apache-2.0** | **Fork + thin rebasable overlay** (already done) | Reuse: the ~800-file inference engine (`tensorzero-*`) — routing, provider fan-out, streaming, optimizers, MCP. Build: Railyard overlay only — `relay-billing`, `relay-catalog`, `relay-aitp`, `relay-cost-feed`, `relay-sdk`, `relay-contract`. | **This is the pattern every other app copies.** Keep vendored crates untouched; all Railyard value in owned overlay crates so the fork stays rebasable (enterprise arch §4, decision 5). The single largest external reuse on the platform. |
| Provider SDKs (openai-go/-node, etc.) | MIT (vendor SDKs) | Adopt as clients | Reuse: `clients/` bindings already vendored. | Keep in lockstep with `relay-contract`. |

### B.5 agent-runtime — paid agent substrate

| Foundation | License | Fork / reference | Reuse vs build | Integration note |
|---|---|---|---|---|
| **rig** (0xPlaygrounds/rig) | **MIT** | **Adopt** (already vendored) | Reuse: `CompletionModel` + `Agent` abstractions — `relay-provider`'s `RelayModel` is a rig `CompletionModel`; `agent-core`'s `PaidCompletionModel` decorates it. Build: the AITP-402 payment seam (`aitp-client`), paid decorators, sessions, tools. | Already the foundation. Target crates `agent-tools`/`agent-session` build on rig's tool + multi-turn model. Keep `aitp-client` free of rig/mint deps (the decoupling that makes it the shared substrate). |
| `async-openai` | **MIT** | Reference / optional | OpenAI-compatible client typings. | Reference for `relay-provider` wire mapping if not using rig's. |

### B.6 vend / abacus / turnstile / ledgerline — the service stack

These four are standard Railyard services; the foundation is the **permissive
async-Rust web stack**, all already in use. No billing/metering OSS is worth
forking — the Mint-settled model is Railyard-specific.

| Foundation | License | Fork / reference | Reuse vs build | Integration note |
|---|---|---|---|---|
| **tokio** | **MIT** | Adopt | Async runtime — all services. | Baseline. |
| **axum** | **MIT** | Adopt | HTTP servers (`*-server`). | Already used across mint/vend/abacus/turnstile/ledgerline/Passport. |
| **tower / tower-http** | **MIT** | Adopt | Middleware — the seam for `turnstile-tower` (generic tower paywall adapter, `turnstile/ARCHITECTURE` target). | `turnstile-axum` exists; `turnstile-tower` generalizes over any tower stack. |
| **sqlx** | **MIT OR Apache-2.0** | Adopt | Postgres stores (`*-store-postgres`) — **runtime query API only** (invariant #6), so crates build with no `DATABASE_URL`. | Platform-wide. Abacus/turnstile scale gaps (`abacus-store-postgres`, `turnstile-nonce-postgres`) are sqlx impls of the store ports. |
| **`reqwest`** + internal **`reqwest-sse-stream`** | reqwest **MIT OR Apache-2.0** | Adopt | Injectable HTTP transport for every `*-client` (mint/relay clients); SSE for streamed inference. | Matches invariant #5 (external I/O behind injectable transport traits). |
| **`argon2` (RustCrypto)** | **MIT OR Apache-2.0** | Adopt | Vend's `vend-keys` provisioning-key hashing (already `argon2id`). | Standard. |
| Billing/metering OSS (e.g. Lago, OpenMeter, Kill Bill) | Lago **AGPL**, OpenMeter **Apache-2.0**, Kill Bill **Apache-2.0** | **Reference only** | Concept reference for usage metering / rating catalogs (`relay-catalog`, `vend-billing`, `ledgerline-rating`). | ⚠️ **Lago is AGPL — avoid.** OpenMeter (Apache-2.0, Go) and Kill Bill (Apache-2.0, Java) are permissively licensed but not Rust; read for the rating/metering data model only. The Mint-settled, success-only billing model is Railyard-owned; there is nothing to fork wholesale. |
| Durable outbox (ledgerline atomicity) | — | Build on internal `durable-tools` | `ledgerline-settlement` durable hold→capture→COGS outbox. | Reuse the internal Apache-2.0 `durable-tools`/`durable-tools-spawn` rather than an external workflow engine (Temporal is MIT but heavy/Go-server). |

### B.7 ranguard — integrity / attestation

Replace the demo's hand-rolled crypto with audited crates; keep the safe-userspace
posture. C2PA connects ranguard's signed evidence to the provenance story.

| Foundation | License | Fork / reference | Reuse vs build | Integration note |
|---|---|---|---|---|
| **RustCrypto: `sha2`, `hmac`, `subtle`** | **MIT OR Apache-2.0** | **Adopt** | Reuse: the audited crypto path `ranguard/ARCHITECTURE` R0 calls for (`rg-crypto`), replacing the demo's from-scratch SHA-256/HMAC (kept as vector-verified reference behind a feature). `subtle` for constant-time compares. | Direct fit; the demo README itself flags shared-secret HMAC as demo-only. |
| **`ed25519-dalek`** | **BSD-3-Clause** | **Adopt** | Reuse: real manifest signatures (`SIGNED_BY` edge, R0 gate). Build: key mgmt (`rg-keys`). | ⚠️ BSD-3 (permissive, not MIT/Apache) — record in `deny.toml`. Same signing basis as Mint receipts → consistent conventions across the rail. |
| **`rand`/`rand_chacha` (CSPRNG)** | **MIT OR Apache-2.0** | **Adopt** | Reuse: replaces the demo's SplitMix64 nonces (README-flagged limitation). | For anti-replay nonces (`rg-heartbeat`). |
| **rustls** (+ `ring` or `aws-lc-rs` backend) | rustls **Apache-2.0 OR MIT OR ISC**; **`ring` has its own unique (ISC-style/OpenSSL-derived) license**; aws-lc-rs **ISC/Apache-2.0** | **Adopt** | Reuse: TLS for `rg-server`/`rg-protocol` transport. | ⚠️ `ring`'s license is **not** MIT/Apache — it's a bespoke permissive license (OK for use, note it). Prefer the `aws-lc-rs` backend if you want cleaner licensing. |
| **`c2pa-rs`** (contentauth) | **Apache-2.0 OR MIT** | **Adopt** | Reuse: express a ranguard `Verdict`/attestation as a signed C2PA-style provenance artifact referenceable by Passport/Mint (arch §6). | Shared with Passport (below) — one provenance library across the rail. |
| **TPM / attestation: `tss-esapi`, `tpm2-tss`; DCAP/SGX** | `tss-esapi` **MIT OR Apache-2.0**; `tpm2-tss` **BSD-2**; Intel SGX DCAP **BSD-3** | **Reference / optional adopt** | Reuse (if hardware attestation is ever in scope): TPM quote verification. Build: ranguard stays **safe userspace, no kernel driver** (invariant) — hardware attestation is an *optional* future tier, not the core. | ⚠️ Keep the `#![forbid(unsafe_code)]` + no-kernel-driver posture. TPM libs are only for an opt-in enterprise attestation add-on; they do not change the core safety stance. |

### B.8 Passport — universal AI identity

Passport maps cleanly onto permissive OSS for provenance, ids, and the
consistency moat. **The dominant license risk is model *weights*, which are
frequently non-commercial even when the code is MIT/Apache.** Map is keyed to the
22-crate target in `context-passport/ARCHITECTURE.md` §3.

| Passport crate | Foundation | License | Fork / ref | Reuse vs build | Integration note |
|---|---|---|---|---|---|
| `passport-core` / ids | **internal `bip39-uuid-substitution`** | Apache-2.0 | Adopt (internal) | Human-readable `PassportId`/`VersionId`/`GenerationId`. | Ids appear in prompts + provenance; word-triples survive LLM round-trips. |
| `passport-provenance` | **`c2pa-rs`** (contentauth) | **Apache-2.0 OR MIT** | **Adopt** | Reuse: C2PA manifests, assertions, **JUMBF export, signing**. Build: passport-specific assertions (digest hard-binding to locked version), Merkle anchoring. | Turns today's "manifest *shape*" (arch §2.4) into a real signed credential (Sx4). Directly closes the deferred C2PA gap. Shared with ranguard. |
| `passport-provenance` (sign/anchor) | **`ed25519-dalek`** + **`rs_merkle`/`r0-merkle`** | dalek **BSD-3**; merkle crates **MIT** | Adopt | Ed25519 signing + Merkle batching (`Anchor`). | ⚠️ dalek BSD-3 note (as above). |
| `passport-consistency` **(THE MOAT)** | **`ort`** (pykeio ONNX Runtime bindings) over **ONNX Runtime** | `ort` **MIT OR Apache-2.0**; ONNX Runtime **MIT** | **Adopt (runtime)** | Reuse: run face/voice/style embedding models for similarity scoring (`FaceScore`/`VoiceScore`/`StyleScore`), drift detection. Build: the scoring policy, seed bank, eval harness, regenerate loop. | The moat's *runtime* is off-the-shelf; the *judgment logic* is Railyard-owned. `ort` is the standard permissive Rust ONNX runtime. |
| `passport-consistency` (face models) | **InsightFace / ArcFace** (embeddings); alt **`facenet-pytorch`** | Code **MIT**; **⚠️ pretrained weights (buffalo_l, antelopev2, VGGFace2) are RESEARCH / NON-COMMERCIAL** | **Reference — weights are the risk** | Reuse: ArcFace architecture as the face-similarity approach. Build/acquire: a **commercially-licensed** face-embedding model, or train/license one. | ⚠️ **Biggest single license trap in Passport.** InsightFace *code* is MIT but its *models* are explicitly non-commercial. Do **not** ship insightface pretrained weights in a commercial product. Options: license a commercial model, use CLIP-family (see below), or train on licensed data. **Flag for owner decision.** |
| `passport-consistency` (style/image similarity) | **CLIP / OpenCLIP** | Code **MIT**; **OpenCLIP weights vary — LAION weights are permissive-ish; verify per checkpoint** | Adopt (code) + verify weights | Reuse: image/style embedding for `StyleScore` and generic identity similarity. | OpenCLIP (MIT code) is a safer commercial path than insightface for style/composite scoring; still verify the *checkpoint* license. |
| `passport-consistency` (voice similarity) | **`speechbrain`/ECAPA-TDNN** speaker embeddings; **Resemblyzer** | SpeechBrain **Apache-2.0**; Resemblyzer **Apache-2.0**; **weights vary** | Reference / adopt | Reuse: speaker-embedding cosine similarity for `VoiceScore`. | Verify the specific speaker-verification checkpoint's license. |
| `passport-watermark` | **`invisible-watermark`** (ShieldMnt); **TrustMark** (Adobe, contentauth) | invisible-watermark **MIT**; **TrustMark — verify (Adobe repo, MIT-ish but confirm)** | Adopt (invisible-watermark) + verify (TrustMark) | Reuse: DWT-DCT invisible watermark embed/detect (`Embedder`/`Detector`). Build: bind payload to `passport_id/version/generation`. | ⚠️ invisible-watermark is Python (MIT) — either bind via `passport-sdk-python` or reimplement the DWT-DCT scheme in Rust. Confirm TrustMark's license before use. Pairs with C2PA for survivability (arch §8.4). |
| `passport-artifacts` (LoRA/diffusion) | **`diffusers`**, **`peft`**, **kohya-ss/sd-scripts**; Rust: **`candle`** | diffusers/peft **Apache-2.0**; sd-scripts **Apache-2.0**; candle **MIT OR Apache-2.0** | Reference (train) / adopt (candle) | Reuse: LoRA training pipelines (`training/lora`) via diffusers/peft; `candle` for Rust-side inference. Build: the artifact registry/broker/minting-policy. | ⚠️ Training *code* is Apache-2.0, but **base model weights (SDXL, Flux, etc.) carry their own licenses** — SDXL is permissive (CreativeML OpenRAIL-ish with use restrictions), Flux dev is **non-commercial**. Passport's arch recommends **broker-first, build-later** (§8.2) — brokering (ElevenLabs voice clone, hosted LoRA) sidesteps most weight-licensing exposure. |
| `passport-artifacts` (voice clone) | **OpenVoice** (MyShell); **Coqui TTS (XTTS)** | OpenVoice **MIT**; **Coqui TTS MPL-2.0; XTTS weights = Coqui non-commercial license** | Reference — weights risk | Reuse: voice-clone approach if self-hosting. | ⚠️ Coqui *code* is MPL-2.0 (weak copyleft, usable) but **XTTS weights are non-commercial**. OpenVoice is MIT. For commercial: **broker** (ElevenLabs/Cartesia APIs) per the arch's Family-B model — the resolver already returns `needs_artifact` and the broker satisfies it. |
| `passport-resolve` / `passport-providers` | provider vendor SDKs | vendor MIT | Adopt as clients | Family-A/B routing to Veo/Runway/Kling/HeyGen/Hedra/ElevenLabs/Cartesia/gpt-image/Flux/Imagen. | Provider adapters are thin clients; no fork. |
| `passport-server` / stores / clients | axum + sqlx + reqwest + subtle | MIT / (sqlx dual) | Adopt | Same standard stack as the other services. | `subtle` already used for constant-time `X-API-Key` (arch §2.1). |
| `passport-sdk-ts` (priority) | typegen from OpenAPI; TensorZero's `-ts-types` pattern | Apache-2.0 (internal pattern) | Reference | TS SDK for Menivor (TS-first, §8.6). | Reuse the relay client-generation pattern. |

---

## Part C — License risk register (explicit flags)

| Project / asset | License | Verdict | Why |
|---|---|---|---|
| **Forgejo** | GPLv3 | **AVOID code reuse** | Copyleft would infect rithub. Use Gitea (MIT) instead. |
| **libgit2** (under `git2`) | GPLv2 + linking exception | **Prefer gitoxide** | Not MIT/Apache; heavier than `gix`'s dual permissive. Fallback/oracle only. |
| **Lago** (billing) | AGPL | **AVOID** | Network-copyleft; toxic for a SaaS rail. Use OpenMeter/Kill Bill (Apache-2.0) as *reference*. |
| **Firefly III** (ledger) | AGPL | **AVOID** | Reference Ledger-cli (BSD) or TigerBeetle (Apache-2.0) instead. |
| **Beancount / hledger** | GPLv2 / GPLv3 | **Reference only, no code copy** | Plaintext-accounting tools; read the model, don't lift code. |
| **InsightFace / ArcFace pretrained weights** | **Non-commercial (research)** | **AVOID in commercial product** | Code is MIT but *weights* are research-only. Biggest Passport trap. License a commercial model or use OpenCLIP. |
| **Flux.1-dev, XTTS weights** | Non-commercial | **AVOID self-hosting commercially** | Base/clone *weights* restrict commercial use even where training code is Apache/MPL. **Broker instead** (Passport §8.2). |
| **`ring`** | Bespoke (ISC-style/OpenSSL-derived) | **OK, note it** | Permissive but not MIT/Apache; consider `aws-lc-rs` backend for cleaner licensing. |
| **`ed25519-dalek`, curve25519-dalek** | BSD-3-Clause | **OK, note it** | Permissive, not MIT/Apache — just record accurately in `deny.toml`. |
| **`comrak`, `tpm2-tss`** | BSD-2 | **OK, note it** | Permissive, not literally MIT/Apache. |
| **Coqui TTS code** | MPL-2.0 | **OK (weak copyleft)** | File-level copyleft; usable, but modifications to MPL files must be shared. Weights are the real block. |
| **TrustMark (Adobe watermarking)** | Verify at adoption | **CONFIRM before use** | Believed MIT on `adobe/trustmark` but confirm the exact repo/commit. |

**Operational guardrail:** every app already carries (or should carry) a
`deny.toml` (present in `apps/relay`, planned for rit per its arch) with a
`cargo-deny` license allow-list. Recommended allow-list: `MIT`, `Apache-2.0`,
`BSD-2-Clause`, `BSD-3-Clause`, `ISC`, `Unicode-DFS-*`, `CC0-1.0`, `Zlib`, plus an
explicit review gate for `MPL-2.0`. Deny `GPL-*` and `AGPL-*`. Model weights are
**not** covered by `cargo-deny` (they're data, not crates) — track them in a
separate `MODEL_LICENSES.md` per the owner decision on build-vs-broker.

---

## Part D — Verification notes

- **Internal (Part A):** grounded in the actual crates — `apps/relay/Cargo.toml`
  (`license = "Apache-2.0"`), `apps/relay/LICENSE`, and each crate's own
  `src/lib.rs` doc header (read directly for `bip39-uuid-substitution`,
  `durable-tools`, `durable-tools-spawn`, `ts-executor-pool`, `provider-proxy`,
  `reqwest-sse-stream`, `minijinja-utils`, `googletest-matchers`, `config-applier`,
  `relay-hedge`). 36 crates confirmed by directory count.
- **External (Part B/C):** licenses stated from the projects' well-known
  licensing. **Pin and re-verify at adoption** (a project can relicense — Forgejo
  did). The high-risk items (model weights, `ring`, dalek/BSD, GPL forges) are
  flagged rather than assumed.
- App target shapes are grounded in the in-repo architecture docs:
  `docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md`, `apps/rit/ARCHITECTURE.md`,
  `apps/rithub/ARCHITECTURE.md`, `apps/ranguard/ARCHITECTURE.md`,
  `apps/context-passport/ARCHITECTURE.md`.
