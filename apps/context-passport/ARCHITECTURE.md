# Passport — Enterprise Architecture

> **Universal AI identity & consistency platform.**
> Lock an identity once — a character, a brand, a persona, a voice, a media style
> (including anime and other stylized aesthetics) — and reproduce it *consistently*
> on **any** provider, for **any** agent, **forever**.
>
> This document is the authoritative enterprise architecture. It supersedes the
> planning notes in `PLAN.md` (the old OAuth-context concept) and expands the
> shipped Sx0 build described in the prior 222-line architecture into the full
> ~22-crate enterprise target. It is deliberately grounded in **what already
> exists** (a real, tested 9-crate Rust workspace) before it projects the target,
> so the reader can always separate *is* from *will be*.

---

## Table of contents

1. [Vision](#1-vision)
2. [Current state — what is real today](#2-current-state--what-is-real-today)
3. [The full enterprise workspace (~22 crates)](#3-the-full-enterprise-workspace-22-crates)
4. [§ Domain ontology — Passport as an identity knowledge graph](#4--domain-ontology--passport-as-an-identity-knowledge-graph)
5. [§ Task graphs — generate-AS-passport as a diamond](#5--task-graphs--generate-as-passport-as-a-diamond)
6. [§ Interop — Passport on the Railyard rail](#6--interop--passport-on-the-railyard-rail)
7. [§ Scale reality — grounded against mint](#7--scale-reality--grounded-against-mint)
8. [§ Owner decisions](#8--owner-decisions)
9. [Appendix A — glossary](#appendix-a--glossary)
10. [Appendix B — the done ladder, restated](#appendix-b--the-done-ladder-restated)

---

## 1. Vision

### 1.1 The problem

Every AI product that generates content hits the same wall the second time it
tries to generate *the same thing*. The model that drew a character yesterday
will not draw the same character today. The provider that cloned a voice cannot
share that clone with the provider that renders the video. A brand's palette and
logo drift the moment a new model or a new prompt author touches them. An agent
that is supposed to *be* a persona forgets who it is between sessions and between
providers. Consistency is treated as a prompt-engineering accident rather than a
durable, ownable asset.

The industry has money infrastructure (a ledger, receipts, settlement) but no
**identity infrastructure**. There is no durable, portable, provider-agnostic,
legally-grounded, provenance-bearing record of *what makes an AI-generated thing
itself* — and no mechanism that reproduces that thing faithfully across every
model provider that exists now or ships next year.

### 1.2 The thesis

**Passport is to AI identity what Mint is to AI money.** Mint is the durable
money asset — an integer-microdollar ledger with receipts and settlement.
Passport is the durable **identity asset** — a typed, versioned, lockable,
content-addressed record of an identity that resolves into per-provider
conditioning and carries its consistency, rights, and provenance with it
everywhere it goes.

The unit of value is the **locked version**: an immutable, content-addressed
(`ni:///sha-256;…`) snapshot of an identity. Once locked, it never changes; new
edits fork a new draft. Every output ever generated from it records exactly which
passport, which locked version, and which provider conditioned it. That record is
the join key between an identity and everything that happens to it downstream —
its consistency scores, its provenance credentials, its money, and its royalties.

### 1.3 Universal in two dimensions

Passport is universal along **two** axes at once, and both matter:

- **Universal across *what* has an identity.** Not just chat-agent personas.
  Passport locks:
  - **Characters** — face/appearance reference images, voice ID, avatar/LoRA/
    embedding refs, body/wardrobe, style, personality/tone.
  - **Brands** — logo, palette, typography, watermark, voice-and-tone guidelines,
    disclaimers.
  - **Personas** — audience/segment preferences, language, target-platform
    conventions, converting hooks.
  - **Voices** — the clone token plus tone/prosody directives, treated as a
    first-class, non-portable trained artifact.
  - **Media styles** — a reproducible aesthetic (anime, cel-shade, photoreal,
    a studio look, a house illustration style) captured as reference set +
    style LoRA + negative constraints, so "draw it in *our* style" is a locked,
    resolvable thing rather than a prompt someone remembers.

- **Universal across *who and what* consumes an identity.** Every AI **agent**
  and every AI-**generating** surface on the rail can "wear" a passport: the
  Menivor reel pipeline, a Vend white-label tenant, a Turnstile-gated paid tool,
  an Abacus-budgeted agent, an `agent-runtime` embodiment. One passport,
  consistent output, on Veo / Runway / Kling / HeyGen / Hedra / ElevenLabs /
  Cartesia / gpt-image / Flux / Imagen / a local LoRA — and on whatever ships
  next quarter.

### 1.4 The one-sentence promise

> **Lock once, reproduce consistently on any provider forever — and settle,
> credential, and (optionally) collect royalties on every reproduction.**

### 1.5 Why this is rail infrastructure, not a product feature

Consistency is not a Menivor feature any more than settlement is a Menivor
feature. It is shared infrastructure that any product on the rail consumes,
that is metered through Mint, measured through Ledgerline, and executed through
Relay. Building it once, correctly, as infrastructure — with a domain kernel, a
resolver, a consistency moat, provenance, and rights — is the entire point.

---

## 2. Current state — what is real today

This section is deliberately conservative. It documents the **shipped Sx0
slice**: a real, compiling, tested multi-crate Rust workspace — not the target.

### 2.1 The 9 crates that exist

`apps/context-passport` is a standalone cargo workspace (`[workspace]`,
`members = ["crates/*"]`), **excluded** from the repo-root GPUI/Metal desktop
build graph, exactly like `apps/mint`, `apps/vend`, and `apps/abacus`. Build and
test from inside the directory only (`cd apps/context-passport && cargo test`).

| # | crate | what is real today |
|---|---|---|
| 1 | `passport-core` | Domain kernel, **no I/O**. `Passport`, `PassportVersion`, `VersionStatus`, typed `PassportAssets` (character/brand/persona) + `AssetsPatch` merge, `Rights` (`ConsentBasis`, `DisclosureLevel`), `VersionDigest` content-addressing, `CreatePassport`/`PassportView`/`PassportFilter`, and the injectable ports `PassportStore` / `RelayConditioningSink` / `MintMeter` with their request/result DTOs (`RelayGenerationRequest`/`Result`, `MintMeterRequest`/`Result`, `CreateOutcome`) plus `PassportError`. |
| 2 | `passport-resolve` | **The novel core.** `Modality` (Image/Voice/Video/Text), `ConditioningPayload` (provider-neutral), `IdentityMechanism` + `ProviderCapability` (the Family-A vs Family-B model), `ProviderAdapter`, `ProviderRegistry`, and `resolve(version, provider, modality)`. Fail-closed: unlocked never resolves; a Family-B provider missing its artifact returns **`needs_artifact`**; an empty payload returns **`resolve_unavailable`**. Per-modality adapters for image/voice/video/text are real, not stubs. |
| 3 | `passport-provenance` | C2PA-aligned lineage: `GenerationRecord` (frozen rights snapshot), a `ProvenanceLog` port + in-memory impl (`store.rs`), and a `to_c2pa_manifest` exporter (`manifest.rs`). |
| 4 | `passport-store` | `MemoryStore` implementing the persistence ports for tests and embedded demos. |
| 5 | `passport-store-postgres` | Neon/Postgres `PostgresStore` via the sqlx **runtime** query API (compiles with no `DATABASE_URL`) + `migrations/0001_passports.sql`. |
| 6 | `passport-relay-client` | Real `reqwest` `RelayConditioningSink` — submits resolved conditioning to Relay (faked in tests). |
| 7 | `passport-mint-client` | Real `reqwest` `MintMeter` — settles passport-conditioned generations on Mint (faked in tests). |
| 8 | `passport-server` | `axum` API + `PassportService` wiring store + provenance + Relay + Mint from env; constant-time `X-API-Key` auth (`subtle`); handlers for passports/versions/resolve/generate/health; fail-closed env config; embedded startup migration. |
| 9 | `passport-testkit` | `FakeRelay` / `FakeMint` transport fakes and fixtures shared across tests. |

**Dependency direction is a strict DAG rooted at `passport-core`; nothing depends
on `passport-server`.**

### 2.2 The core loop that runs today

```
create → edit(draft) → lock(immutable + content-addressed)
       → version(fork from a locked one)
       → resolve(provider, modality)
       → generate AS passport (Relay + provenance + Mint)
       → audit
```

Two facts from the provider research already shape the code:

1. **Identity splits into two families.** *Family A* is portable inference-time
   reference (reference images, persona text — resent each call). *Family B* is a
   non-portable per-provider trained artifact (LoRA, HeyGen/Hedra avatar, **every**
   voice clone). The resolver routes a neutral passport into whichever family the
   target provider supports and returns `needs_artifact` when a Family-B provider
   lacks its minted token — rather than silently degrading consistency.
2. **Locking content-addresses the identity.** Locking canonicalizes the version
   body and stamps a `ni:///sha-256;…` digest; that digest is the version's
   identity and is frozen into every generation's provenance (C2PA hard-binding).

### 2.3 Measured size of the shipped slice

| metric | value |
|---|---:|
| crates | **9** |
| `.rs` files | **48** |
| lines of Rust | **~3,731** |
| test functions | **26** (across 8 files), all passing |

### 2.4 What is explicitly deferred (Sx1+)

The resolver contract, capability routing, versioning/lock integrity,
provenance, and the Relay/Mint seams are real and tested with fakes. Deferred and
tracked as explicit extension points:

- the **live** Relay conditioning-endpoint contract and the Mint metering endpoint;
- the per-provider `provider_artifacts` minting/registry subsystem (training);
- **consistency scoring / drift detection / eval harness** (the moat — none of it
  exists yet);
- Ed25519 signing + Merkle anchoring of locked versions and a real C2PA JUMBF
  exporter (today it is a manifest *shape*, not a signed credential);
- `passport_grants` cross-product access control;
- rights/deepfake enforcement beyond the `Rights` value type;
- watermarking, personalization, Ledgerline margin emission, the wire protocol
  crate, and the language SDKs;
- the Vend / Turnstile / Abacus / agent-runtime distribution surfaces.

> **Honesty gate:** the Sx0 slice is a *real spine*, not a finished platform. The
> [done ladder](#appendix-b--the-done-ladder-restated) below exists precisely to
> stop anyone from calling these 9 crates "Passport, done."

---

## 3. The full enterprise workspace (~22 crates)

The target expands the current 9 crates into **~22**. The DAG discipline is
preserved: kernel → ports → adapters → services → server → SDKs; nothing depends
on `passport-server`; every outbound HTTP call goes through an injectable trait
declared in `passport-core` (or `passport-protocol`) and faked in tests; sqlx
uses the runtime query API only.

### 3.1 The dependency layers

```
                         ┌─────────────────────────────────────────┐
  L0  kernel             │ passport-core                           │
                         └───────────────┬─────────────────────────┘
                                         │
  L1  domain modules      passport-assets · passport-rights · passport-protocol
                                         │
  L2  the hard cores      passport-resolve · passport-consistency (THE MOAT)
                          passport-artifacts · passport-provenance · passport-watermark
                          passport-personalize · passport-audit
                                         │
  L3  ports + adapters    passport-store · passport-store-postgres
                          passport-providers (+5 provider-family adapters)
                          passport-relay-client · passport-mint-client
                          passport-ledgerline-client
                                         │
  L4  service/binary      passport-server (axum data + control plane)
                                         │
  L5  edge / consumers    passport-sdk-rust · passport-sdk-python · passport-sdk-ts
  Lx  cross-cutting       passport-testkit  (fakes for every port, all layers)
```

### 3.2 Crate-by-crate — responsibility, key types, modules

Crates marked **(exists)** are grown from the shipped slice; **(new)** are added
to reach the enterprise target.

---

#### 3.2.1 `passport-core` **(exists)**
**Responsibility:** the domain kernel — every invariant that must hold regardless
of I/O. No async, no HTTP, no DB.
**Key types:** `PassportId` / `VersionId` / `GenerationId` newtypes, `PassportKind`,
`Passport`, `PassportVersion`, `VersionStatus`, `VersionDigest`, `CreatePassport`,
`PassportView`, `PassportFilter`, the ports `PassportStore` / `RelayConditioningSink`
/ `MintMeter`, port DTOs (`CreateOutcome`, `RelayGeneration{Request,Result}`,
`MintMeter{Request,Result}`), `PassportError`.
**Modules:** `ids`, `kind`, `version`, `passport`, `digest`, `ports`, `error`,
`clock` (injectable time), `id_alloc` (injectable id source).

#### 3.2.2 `passport-assets` **(new)**
**Responsibility:** the typed, versioned identity fields and their merge/validation
semantics, extracted out of `core` so the asset taxonomy can grow (voice,
media-style, anime) without bloating the kernel. This is the schema of *what an
identity is*.
**Key types:** `PassportAssets` (enum over character/brand/persona/voice/style),
`AssetsPatch`, `AssetRef`, `ReferenceSet`, `StyleProfile`, `VoiceProfile`,
`MediaStyle` (photoreal | anime | cel | house-style …), `AssetValidation`.
**Modules:** `character`, `brand`, `persona`, `voice`, `style`, `patch` (immutable
merge), `refs` (content-addressed asset references), `validate`.

#### 3.2.3 `passport-rights` **(new)**
**Responsibility:** consent, likeness, disclosure, licensing, and **deepfake
posture** as enforceable domain logic — not just a struct. Gate `lock` and
`generate-AS` on rights.
**Key types:** `Rights`, `ConsentBasis`, `DisclosureLevel`, `LicenseTerms`,
`LikenessGrant`, `DeepfakePolicy`, `RightsDecision` (allow / deny / require-disclosure).
**Modules:** `consent`, `license`, `likeness`, `disclosure`, `deepfake`, `evaluate`.

#### 3.2.4 `passport-protocol` **(new)**
**Responsibility:** the shared **wire protocol** — request/response envelopes,
the AITP-402 royalty message shape, error taxonomy, and the versioned schemas
that every SDK and every sibling service serialize against. Single source of
truth for the on-the-wire contract.
**Key types:** `ResolveRequest`/`ResolveResponse`, `GenerateAsRequest`/`Response`,
`Envelope<T>` (success/data/error/meta), `Aitp402RoyaltyIntent`, `ProtocolError`,
`SchemaVersion`.
**Modules:** `envelope`, `resolve`, `generate`, `royalty`, `errors`, `versioning`.

#### 3.2.5 `passport-resolve` **(exists)**
**Responsibility:** turn a **locked** version into provider-specific conditioning
— the key infrastructure method. Fail-closed.
**Key types:** `Modality`, `ConditioningPayload`, `IdentityMechanism`,
`ProviderCapability`, `ProviderAdapter`, `ProviderRegistry`, `resolve()`,
`ResolveOutcome` (`Resolved` | `NeedsArtifact` | `Unavailable`).
**Modules:** `modality`, `payload`, `capability`, `adapter`, `registry`,
`adapters/{image,voice,video,text,style}`, `negotiate` (capability fallback).

#### 3.2.6 `passport-providers` **(new)**
**Responsibility:** the concrete, per-**provider-family** adapters that
`passport-resolve` routes into. Extracts vendor-specific mapping/quirks out of the
generic resolver so adding a provider is adding a module, not editing the core.
Ships **five provider-family adapters**:
1. `family_image` — gpt-image / Flux / Imagen / SDXL-LoRA (reference + embedding + LoRA).
2. `family_video` — Veo / Runway / Kling (first-frame / reference-driven).
3. `family_avatar` — HeyGen / Hedra (enrolled talking-head avatars — Family B).
4. `family_voice` — ElevenLabs / Cartesia (voice-clone tokens — Family B).
5. `family_style` — style/LoRA hosts for locked media styles incl. anime (Family B).
**Key types:** `ProviderFamily`, `FamilyAdapter` (impls `ProviderAdapter`),
`ProviderId`, `CapabilityMatrix`, `ArtifactRequirement`.
**Modules:** `family_image`, `family_video`, `family_avatar`, `family_voice`,
`family_style`, `matrix`, `registry_ext`.

#### 3.2.7 `passport-consistency` **(new) — THE MOAT**
**Responsibility:** answer the only question that matters — *did the output
actually stay on-identity?* — and act on the answer. This is what makes Passport
defensible; anyone can store a struct, but measuring and enforcing cross-provider
consistency is the durable advantage.
**Key types:** `FaceScore`, `VoiceScore`, `StyleScore`, `ConsistencyScore`
(composite + per-dimension), `ConsistencyBar` (best-effort | verified),
`DriftReport`, `DriftTrend`, `Seed`, `SeedBank`, `EvalCase`, `EvalReport`,
`RegenerateDecision`.
**Modules:**
- `scoring/{face,voice,style,composite}` — similarity scoring per modality against
  the locked reference set;
- `drift` — per-passport/version drift detection over a rolling window;
- `seed_bank` — remembers the seeds/params that produced high-consistency outputs
  per provider, so reproduction is deterministic where the provider allows;
- `eval` — the offline eval harness (fixtures + golden references + pass/fail
  thresholds) that gates a version as "verified on provider P";
- `policy` — turn a score + the owner's bar into `Resolved` / `Regenerate` /
  `NeedsArtifact` / `Reject`;
- `ports` — `ConsistencyScorer` trait (injectable model backends, faked in tests).

#### 3.2.8 `passport-artifacts` **(new)**
**Responsibility:** the Family-B lifecycle — **train / mint / register / broker**
per-provider trained artifacts (LoRAs, voice clones, avatars, style models) so
`needs_artifact` can be *satisfied* instead of merely reported.
**Key types:** `TrainedArtifact`, `ArtifactKind` (Lora | VoiceClone | Avatar |
StyleModel), `ArtifactStatus` (Requested | Training | Ready | Failed | Revoked),
`TrainingJob`, `ArtifactRegistry`, `MintingPolicy` (build | broker | reference-only).
**Modules:** `registry`, `training/{lora,voice,avatar,style}`, `job` (async queue),
`policy`, `ports` (`ArtifactTrainer` trait — external training backends, faked).

#### 3.2.9 `passport-provenance` **(exists)**
**Responsibility:** C2PA-aligned lineage — bind every output to its passport,
locked version digest, provider, consistency score, and rights snapshot; export a
signed, verifiable credential.
**Key types:** `GenerationRecord`, `ProvenanceManifest`, `AuditAssertion`,
`C2paManifest`, `SigningKey`, `Anchor` (Merkle root), `to_c2pa_manifest()`.
**Modules:** `record`, `manifest`, `store`, `sign` (Ed25519), `anchor`
(Merkle batching), `verify`, `c2pa` (JUMBF export).

#### 3.2.10 `passport-watermark` **(new)**
**Responsibility:** embed and detect an invisible, passport-bound watermark in
generated media, so provenance survives re-hosting and screenshotting and a
leaked asset can be traced back to `passport_id/version/generation`.
**Key types:** `WatermarkPayload` (carries digest + generation id), `Embedder`,
`Detector`, `WatermarkResult`, `Robustness`.
**Modules:** `embed`, `detect`, `payload`, `image`, `video`, `ports`
(`WatermarkBackend` trait, faked).

#### 3.2.11 `passport-personalize` **(new)**
**Responsibility:** the persona/audience layer — resolve per-audience, per-language,
per-platform variants of a locked persona and feed the converting-hook loop, *without*
mutating the locked identity (variants are overlays, not edits).
**Key types:** `AudienceSegment`, `PersonalizationOverlay`, `HookSet`, `PlatformProfile`,
`LanguageProfile`, `PersonalizedConditioning`.
**Modules:** `segment`, `overlay`, `hooks`, `platform`, `language`, `resolve_ext`.

#### 3.2.12 `passport-audit` **(new)**
**Responsibility:** the append-only, tamper-evident audit spine for the whole
service — who created/edited/locked/forked/resolved/generated/granted/revoked what,
when, and why. An audit-write failure blocks the operation rather than serving
un-receipted action.
**Key types:** `AuditEvent`, `AuditKind`, `Actor`, `AuditLog` (port), `HashChain`,
`AuditQuery`.
**Modules:** `event`, `kinds`, `chain` (hash-linked), `query`, `ports`.

#### 3.2.13 `passport-store` **(exists)**
**Responsibility:** the persistence ports + an in-memory implementation for tests
and embedded demos.
**Key types:** `MemoryStore` (impls `PassportStore`, `ProvenanceLog`, `AuditLog`,
`ArtifactRegistry`).
**Modules:** `memory`, `index`, `fixtures`.

#### 3.2.14 `passport-store-postgres` **(exists)**
**Responsibility:** the Neon/Postgres source of truth via the sqlx runtime API +
migrations. Compiles with no `DATABASE_URL`.
**Key types:** `PostgresStore`, `PgProvenanceLog`, `PgArtifactRegistry`, `PgAuditLog`.
**Modules:** `store`, `rows`, `migrate`, `provenance`, `artifacts`, `audit`,
`migrations/000{1..N}_*.sql`.

#### 3.2.15 `passport-relay-client` **(exists)**
**Responsibility:** submit resolved `ConditioningPayload` to Relay for provider
fan-out.
**Key types:** `RelayClient` (impls `RelayConditioningSink`), `RelayConfig`.
**Modules:** `client`, `config`, `retry`, `map` (protocol ⇄ Relay wire).

#### 3.2.16 `passport-mint-client` **(exists)**
**Responsibility:** meter passport-conditioned generations on Mint (success-only,
ADR-0002) and initiate AITP-402 royalty transfers.
**Key types:** `MintClient` (impls `MintMeter`), `Aitp402Transfer`, `MintReceiptRef`.
**Modules:** `client`, `config`, `meter`, `royalty`.

#### 3.2.17 `passport-ledgerline-client` **(new)**
**Responsibility:** emit a per-generation **margin** event to Ledgerline tagged by
`passport_id`/`version`/`kind` for per-character / per-brand gross-margin analytics.
**Key types:** `LedgerlineClient`, `MarginEvent`, `LedgerlineConfig`.
**Modules:** `client`, `config`, `event`.

#### 3.2.18 `passport-server` **(exists)**
**Responsibility:** the axum data + control plane binary that wires every port
from env, fail-closed. The only crate that depends on everything.
**Key types:** `AppState`, `PassportService`, `build_router`.
**Modules:** `main`, `lib`, `state`, `auth`, `router`, `dto`, `service`,
`handlers/{passports,versions,resolve,generate,consistency,artifacts,grants,
provenance,health}`, `config`, `error`, `middleware`.

#### 3.2.19 `passport-sdk-rust` **(new)**
**Responsibility:** typed Rust client for embedding Passport into other rail Rust
services (agent-runtime, Abacus).
**Key types:** `Passport` (client), `GenerateAs`, `ResolveHandle`, `SdkError`.
**Modules:** `client`, `generate`, `resolve`, `models`, `error`.

#### 3.2.20 `passport-sdk-python` **(new)**
**Responsibility:** Python client for data/ML and training-pipeline integrations
(artifact training, eval harness authoring).
**Modules (pkg):** `client`, `generate`, `resolve`, `artifacts`, `eval`, `models`.

#### 3.2.21 `passport-sdk-ts` **(new) — priority SDK**
**Responsibility:** the TypeScript SDK — **first-class**, because Menivor (the
first heavy consumer) is TS. `generate AS passport` from a Menivor reel job in a
few lines.
**Modules (pkg):** `client`, `generateAs`, `resolve`, `personalize`, `provenance`,
`models`, `errors`.

#### 3.2.22 `passport-testkit` **(exists)**
**Responsibility:** fakes and fixtures for **every** port at **every** layer, so
the whole graph tests hermetically with no network or DB.
**Key types:** `FakeRelay`, `FakeMint`, `FakeLedgerline`, `FakeScorer`,
`FakeTrainer`, `FakeWatermark`, `MemoryStore` re-exports, golden fixtures.
**Modules:** `relay`, `mint`, `ledgerline`, `scorer`, `trainer`, `watermark`,
`fixtures`, `builders`.

### 3.3 Crate summary table

| # | crate | layer | status | one-line responsibility |
|---|---|---|---|---|
| 1 | `passport-core` | L0 | exists | domain kernel, no I/O — identity invariants |
| 2 | `passport-assets` | L1 | new | the typed identity taxonomy (incl. voice, media-style/anime) |
| 3 | `passport-rights` | L1 | new | consent/likeness/license/deepfake as enforceable logic |
| 4 | `passport-protocol` | L1 | new | wire envelopes, AITP-402 royalty shape, schema versioning |
| 5 | `passport-resolve` | L2 | exists | locked version → provider conditioning, fail-closed |
| 6 | `passport-providers` | L3 | new | 5 provider-family adapters (image/video/avatar/voice/style) |
| 7 | `passport-consistency` | L2 | new | **THE MOAT** — scoring, drift, seed bank, eval, regenerate |
| 8 | `passport-artifacts` | L2 | new | train/mint/broker Family-B artifacts |
| 9 | `passport-provenance` | L2 | exists | C2PA lineage, Ed25519 sign, Merkle anchor |
| 10 | `passport-watermark` | L2 | new | invisible passport-bound watermark embed/detect |
| 11 | `passport-personalize` | L2 | new | audience/language/platform overlays + hooks |
| 12 | `passport-audit` | L2 | new | append-only, hash-chained audit spine |
| 13 | `passport-store` | L3 | exists | ports + in-memory store |
| 14 | `passport-store-postgres` | L3 | exists | Neon/Postgres source of truth |
| 15 | `passport-relay-client` | L3 | exists | submit conditioning to Relay |
| 16 | `passport-mint-client` | L3 | exists | meter + AITP-402 royalty on Mint |
| 17 | `passport-ledgerline-client` | L3 | new | per-generation margin events |
| 18 | `passport-server` | L4 | exists | axum data + control plane binary |
| 19 | `passport-sdk-rust` | L5 | new | Rust client |
| 20 | `passport-sdk-python` | L5 | new | Python client (ML/training) |
| 21 | `passport-sdk-ts` | L5 | new | **TS client — priority for Menivor** |
| 22 | `passport-testkit` | Lx | exists | fakes + fixtures for every port |

---

## 4. § Domain ontology — Passport as an identity knowledge graph

Passport's enterprise data model is not a pile of tables — it is a **knowledge
graph** whose nodes are identity entities and whose edges are typed, time-stamped,
provenance-bearing relationships. Modeling it as a graph is not decoration: the
questions the business must answer ("trace this leaked image", "what do we owe
this creator", "is this character drifting on Kling") are *graph traversals*, and
the schema below is chosen so each one is a bounded walk from a known node.

### 4.1 Entity types (nodes) — ~15

| # | node | identity | key attributes |
|---|---|---|---|
| E1 | **Owner** | principal id | account, org, verified-likeness holder |
| E2 | **Passport** | `PassportId` | kind (character/brand/persona/voice/style), status, head version |
| E3 | **PassportVersion** | `(PassportId, version)` | status (draft/locked), created/locked_at |
| E4 | **AssetBundle** | content hash | the typed asset payload of a version (face refs, palette, voice profile, style) |
| E5 | **Rights** | attached to version | consent basis, disclosure level, license terms, deepfake policy |
| E6 | **VersionDigest** | `ni:///sha-256;…` | the immutable content address of a locked version |
| E7 | **Provider** | provider id | Veo/Runway/Kling/HeyGen/Hedra/ElevenLabs/Cartesia/gpt-image/Flux/Imagen/LoRA-host |
| E8 | **ProviderCapability** | `(Provider, Modality)` | identity mechanism (Family A ref vs Family B artifact), needs_artifact |
| E9 | **ConditioningPayload** | resolve result id | provider-neutral conditioning resolved for a target |
| E10 | **TrainedArtifact** | artifact id | kind (LoRA/voice-clone/avatar/style), status, provider binding |
| E11 | **Generation (Output)** | `GenerationId` | provider, modality, output ref, timestamp |
| E12 | **ConsistencyScore** | attached to generation | face/voice/style dims + composite + bar |
| E13 | **ProvenanceManifest** | manifest id | C2PA assertions, signature, Merkle anchor |
| E14 | **MintReceipt** | `mint_receipt_ref` | the settlement record — **the identity↔money join key** |
| E15 | **RoyaltyTransfer** | AITP-402 transfer id | payer, payee (owner), amount |
| E16 | **Grant** | grant id | who may generate-AS, scope, expiry |
| E17 | **SeedBank entry** | `(Provider, version)` | deterministic seed/params that produced high consistency |
| E18 | **AuditEvent** | event id | actor, kind, time, hash-chain link |
| E19 | **Watermark** | payload id | invisible mark bound to a generation |

(E1–E15 are the core fifteen; E16–E19 are the enterprise extension nodes.)

### 4.2 Typed edges — ~20, each carrying **cardinality + time + provenance**

Every edge records **when** it was asserted and **by which actor/process**
(provenance), and declares its cardinality. This is what makes the graph
auditable: no relationship exists without a timestamp and an author.

| # | edge | from → to | card. | time | provenance |
|---|---|---|---:|---|---|
| R1 | `OWNS` | Owner → Passport | 1:N | created_at | creation request |
| R2 | `HAS_VERSION` | Passport → PassportVersion | 1:N | version created_at | edit/fork op |
| R3 | `FORKED_FROM` | PassportVersion → PassportVersion | N:1 | forked_at | version op |
| R4 | `BUNDLES` | PassportVersion → AssetBundle | 1:1 | at edit | patch merge |
| R5 | `CARRIES` | PassportVersion → Rights | 1:1 | at edit | rights decision |
| R6 | `ADDRESSED_BY` | PassportVersion → VersionDigest | 1:1 | locked_at | lock op (canonical hash) |
| R7 | `HAS_CAPABILITY` | Provider → ProviderCapability | 1:N | matrix updated_at | provider research/registry |
| R8 | `RESOLVES_TO` | PassportVersion → ConditioningPayload | 1:N | resolved_at | resolver run |
| R9 | `TARGETS` | ConditioningPayload → Provider | N:1 | resolved_at | resolver run |
| R10 | `REQUIRES_ARTIFACT` | PassportVersion → TrainedArtifact | N:M | needs_artifact at | resolver (Family-B gap) |
| R11 | `MINTED_FOR` | TrainedArtifact → Provider | N:1 | trained_at | artifact trainer |
| R12 | `PRODUCES` | ConditioningPayload → Generation | 1:N | generated_at | Relay execution |
| R13 | `SCORED_BY` | Generation → ConsistencyScore | 1:1 | scored_at | consistency scorer |
| R14 | `CREDENTIALED_BY` | Generation → ProvenanceManifest | 1:1 | signed_at | provenance signer |
| R15 | `WATERMARKED_BY` | Generation → Watermark | 1:1 | embedded_at | watermark backend |
| R16 | `SETTLED_BY` | Generation → MintReceipt | 1:1 | settled_at | Mint (success-only) |
| R17 | `TRIGGERS` | Generation → RoyaltyTransfer | 0:1 | transferred_at | AITP-402 (3rd-party generate-AS) |
| R18 | `PAYS` | RoyaltyTransfer → Owner | N:1 | transferred_at | Mint transfer |
| R19 | `GRANTS` | Owner → Grant → Principal | 1:N | granted_at / expires_at | grant op |
| R20 | `USED_SEED` | Generation → SeedBank entry | N:1 | generated_at | resolver/seed policy |
| R21 | `EMITS` | (any op) → AuditEvent | 1:N | event_at | append-only spine |
| R22 | `JOINS` | MintReceipt ⇄ Generation | 1:1 | settled_at | `mint_receipt_ref` join key |

### 4.3 The drawn graph

The spine the whole platform is built around — **Owner → Passport → Version →
Output → {scored, credentialed, settled, royalty}**:

```
        (E1) Owner
          │ OWNS (1:N)
          ▼
      (E2) Passport ──HAS_VERSION(1:N)──► (E3) PassportVersion ──FORKED_FROM──┐
                                                │   │   │                     │
                            BUNDLES│    CARRIES │   │   │ ADDRESSED_BY         │(N:1, prior locked)
                                   ▼            ▼   │   ▼                      │
                          (E4) AssetBundle  (E5) Rights  (E6) VersionDigest ◄──┘
                                                    │
                                       RESOLVES_TO  │ (1:N per Provider×Modality)
                                                    ▼
                                        (E9) ConditioningPayload ──TARGETS──► (E7) Provider
                                                    │                              │ HAS_CAPABILITY
                                     REQUIRES_ARTIFACT│(N:M)                        ▼
                                                    ▼                     (E8) ProviderCapability
                                        (E10) TrainedArtifact ─MINTED_FOR─► (E7) Provider
                                                    │
                                          PRODUCES  │ (1:N)   ◄─USED_SEED─ (E17) SeedBank
                                                    ▼
                                            (E11) Generation ─────────────────────────────────┐
                                            │        │          │            │                 │
                                   SCORED_BY│CREDENTIALED_BY│  WATERMARKED_BY│  SETTLED_BY      │TRIGGERS
                                            ▼        ▼          ▼            ▼                 ▼
                                (E12) Consistency  (E13) Prov-  (E19) Water- (E14) MintReceipt (E15) RoyaltyTransfer
                                       Score         Manifest    mark          │  JOINS         │ PAYS
                                    (face/voice/    (C2PA sign, (invisible,    │ (mint_receipt  ▼
                                     style + bar)    Merkle)     traceable)    │  _ref)      (E1) Owner
                                                                              └── identity ↔ money join

   Every node/edge above ──EMITS──► (E18) AuditEvent   (append-only, hash-chained)
```

`mint_receipt_ref` (edge R22 `JOINS`) is the single join key that stitches the
**identity graph** (left/top) to the **money graph** (Mint, right). It is how "this
character" and "this dollar" become one query.

### 4.4 Competency questions (the graph must answer these as bounded walks)

1. **Trace a leak.** *Given a leaked image (or its watermark payload), which
   passport, locked version, provider, and generation produced it, and under what
   rights?* → `Watermark → Generation → {ConditioningPayload→Version→Digest,
   Provider, Rights}` (reverse R15, R12, R8, R6, R5).
2. **Consistency audit.** *What is the face-similarity drift trend for character C
   across its last 500 generations on Kling?* → walk `Passport→Versions→…→
   Generations` filtered by provider, aggregate `SCORED_BY` (R13) over time.
3. **Artifact gaps.** *Which providers currently lack a trained artifact for
   locked version V?* → `Version →REQUIRES_ARTIFACT→ TrainedArtifact` vs
   `Provider` set; the complement is the `needs_artifact` backlog.
4. **Royalty owed.** *Total royalties owed to Owner O for third-party generate-AS
   in Q3?* → `Owner ←PAYS← RoyaltyTransfer ←TRIGGERS← Generation` windowed by
   `settled_at` (R18, R17, R16).
5. **Access review.** *Who currently holds an unexpired grant to generate AS this
   brand?* → `Passport ←GRANTS Grant` filtered `expires_at > now` (R19).
6. **Canonical reference.** *Which locked version is the highest-consistency
   reference for provider P?* → `Passport→Versions→Generations[provider=P]` ranked
   by composite `ConsistencyScore`.
7. **Provenance verification.** *Is this output's credential valid and does its
   digest match the claimed locked version?* → `Generation→ProvenanceManifest`
   verify signature + Merkle anchor, compare to `Version→VersionDigest` (R14, R6).
8. **Money↔identity join.** *For Mint receipt R, which identity did we settle?* →
   `MintReceipt →JOINS→ Generation → … → Passport` (R22).
9. **Rights enforcement.** *Can principal X generate AS this character right now?*
   → `Grant` (R19) ∩ `Rights` deepfake/consent policy (R5) → allow/deny/disclose.
10. **Deterministic reproduction.** *What seed/params reproduce version V on
    provider P?* → `SeedBank[(P,V)]` (R20).

This ontology **is** the enterprise data model. The Postgres schema
(`passports`, `passport_versions`, `provider_artifacts`, `passport_grants`,
`passport_generations`, plus new `consistency_scores`, `royalty_transfers`,
`seed_bank`, `audit_events`, `watermarks`) is a physical projection of these
nodes and edges; a GraphRAG/serving view over the same data answers the
competency questions above for operators and for agents.

---

## 5. § Task graphs — generate-AS-passport as a diamond

Where §4 is the **knowledge graph** (what exists and how it relates), this section
is the **task graph** (how work flows). `generate AS passport` is deliberately
shaped as a **diamond** — split → parallel fan-out → **separate** verification →
owned merge → settle — because that is the shape that gets *consistency* right
across providers while keeping the verifier honest.

### 5.1 The generate-AS diamond

```
                         ┌──────────────────────────────────────────┐
   request:              │  generate AS passport P @ locked V, {providers}, modality  │
   "make this            └───────────────────────┬──────────────────┘
    on-identity"                                  │
                                            (0) SPLIT
                          resolve V for each target provider (fan-out plan)
                                    ┌──────────────┼──────────────┐
                                    ▼              ▼              ▼
                          (1a) resolver:   (1b) resolver:   (1c) resolver:
                           provider A        provider B       provider C
                          → payload / →needs_artifact →resolve_unavailable
                                    │              │              │
                          (2a) Relay exec   (2b) Relay exec  (2c) (skipped/queued
                           → candidate a    → candidate b     for artifact training)
                                    └──────────────┼──────────────┘
                                                   ▼
                                 (3) SEPARATE CONSISTENCY VERIFIER
                                 (passport-consistency — a DIFFERENT task than
                                  the generators; scores face/voice/style vs the
                                  locked reference; NOT allowed to also generate)
                                                   │  scores + pass/fail per candidate
                                                   ▼
                                 (4) OWNED MERGE (orchestrator)
                                 pick best candidate ≥ owner's consistency bar;
                                 below-bar → regenerate (seed bank) or reject
                                                   │
                                                   ▼
                                 (5) PROVENANCE + WATERMARK + SETTLE
                                 sign C2PA manifest, embed watermark,
                                 meter Mint (success-only), trigger royalty,
                                 emit Ledgerline margin, append audit
                                                   │
                                                   ▼
                                            on-identity output
```

**Why this exact shape:**

- **Split → parallel provider-resolvers.** The resolver is pure and
  side-effect-free per provider, so fan-out is safe and the slowest provider does
  not block the others. Each branch independently returns `Resolved`,
  `needs_artifact`, or `resolve_unavailable` — the diamond degrades per-branch, not
  all-or-nothing.
- **Separate consistency-verifier (the key discipline).** The task that *judges*
  consistency is a **different node** from the tasks that *generate*. A generator
  cannot grade its own homework; letting it would collapse the moat into wishful
  self-report. The verifier consumes candidates, scores them against the locked
  reference set, and emits an independent verdict.
- **Owned merge.** A single owner node (the orchestrator, on behalf of the
  passport owner) selects the winning candidate against the **owner's declared
  consistency bar**. Below-bar candidates route to regeneration (seeded from the
  seed bank) or rejection — never silently shipped.
- **Provenance + settle after merge.** Only the merged, accepted output is
  credentialed, watermarked, metered, and (if third-party) royalty-charged. This
  keeps the identity↔money join (`mint_receipt_ref`) attached to exactly one
  canonical output.

### 5.2 The human gate — lock and rights-grant

Two operations are **never** autonomous. They sit behind an explicit human gate,
because they create durable, hard-to-reverse commitments:

```
   draft edits ──► [ HUMAN GATE ] ──► lock (immutable, content-addressed forever)
                        │
   grant request ──► [ HUMAN GATE ] ──► rights-grant (who may generate-AS, scope, expiry)
```

- **Lock** freezes an identity forever; an agent cannot lock on its own authority.
- **Rights-grant** (letting someone else generate AS your likeness/brand) is a
  consent decision with legal weight — it requires the owner, and the
  `passport-rights` deepfake/consent policy must pass.

Everything *upstream* of a lock (drafting, resolving previews, scoring) and
everything *downstream* of an existing grant (generate-AS within scope) can be
autonomous; the two gates are the only mandatory human checkpoints.

### 5.3 The stop rule

The diamond terminates deterministically. A branch stops when it reaches
`Resolved`+accepted (settle), `resolve_unavailable` (drop branch), or
`needs_artifact` (queue for training, do **not** loop-generate). The merge stops
when a candidate clears the bar **or** the regenerate budget is exhausted (then:
reject, with an audit event) — never an unbounded retry loop.

### 5.4 Nesting into sibling task graphs

`generate AS passport` is a **sub-graph** that nests inside larger rail task
graphs; Passport is a node in their diamonds, not a standalone island:

- **Inside Menivor's reel graph.** Menivor's "produce a reel" task graph fans out
  scene generations; each scene calls the Passport diamond via the TS SDK so every
  shot is the same character/brand/style. Passport's merge feeds Menivor's stitch.
- **Inside Relay's fan-out graph.** Passport's step (2) *is* a Relay task; Relay
  owns provider execution and Passport supplies the conditioning node upstream of
  it. Passport does not re-implement provider fan-out — it composes with Relay's.
- **Inside Mint's settlement graph.** Passport's step (5) hands Mint a
  success-only meter; Mint's settlement sub-graph produces the `mint_receipt_ref`
  that Passport joins back onto the generation.

---

## 6. § Interop — Passport on the Railyard rail

Passport is infrastructure: it **uses** three services beneath it and is **used
by** at least five above it. The seam to each is a concrete, typed contract.

### 6.1 What Passport uses (downstream dependencies)

| service | Passport's role | concrete seam |
|---|---|---|
| **Relay** | Passport is an identity **supplier**; Relay is the **executor**. | `passport-relay-client` attaches a resolved `ConditioningPayload` to a Relay inference request (`generate AS passport X`). Relay fans out to the actual provider (Veo/Runway/Kling/HeyGen/Hedra/ElevenLabs/Cartesia/LoRA). |
| **Mint** | Passport is a **spender + royalty initiator**; Mint is the **ledger**. | `passport-mint-client` meters each passport-conditioned generation **success-only** (ADR-0002), tagging the Mint receipt with `passport_id`/`version`. On third-party generate-AS it initiates an **AITP-402** royalty transfer to the owner's Mint account. |
| **Ledgerline** | Passport is a **margin emitter**; Ledgerline is the **lens**. | `passport-ledgerline-client` emits a per-generation margin event tagged `passport_id`/`version`/`kind` → per-character / per-brand gross-margin analytics. |

### 6.2 What uses Passport (upstream consumers)

| consumer | how it "wears" a passport | seam |
|---|---|---|
| **Menivor** | consistent UGC reels — same character/brand/style across every shot | **`passport-sdk-ts`** — `generate AS passport` from a reel job (TS-first, see §8). |
| **Vend** | white-label tenants resell metered, on-identity inference | tenants resolve a passport (HTTP) before calling Relay, so resold generation stays on-identity; Passport is the shared identity layer under Vend's multitenancy. |
| **Turnstile** | `generate AS passport X` exposed as a **paid AITP-402 tool** | Turnstile gates the tool and settles the licensing/royalty fee on Mint. |
| **Abacus** | an agent embodying a character has a **hard, receipted budget** | passport-conditioned generations flow through Abacus so embodiment can't overspend. |
| **agent-runtime** | an agent literally **wears** a passport | resolved persona/voice/text/style conditioning is loaded into the rig `Agent` so it speaks and acts as the locked identity, paying via the AITP-402 seam. |

### 6.3 The join key: `mint_receipt_ref`

Across every seam, the **`mint_receipt_ref`** is the single identifier that joins
the **identity** side (which passport/version/generation) to the **money** side
(which settlement/royalty). It is written into `passport_generations` at settle
time (edge R22 `JOINS`) and is the primitive that makes "what did we earn on this
character?" and "which identity produced this receipt?" the same lookup from
either direction. Every consumer and every downstream service speaks it.

### 6.4 Reused rail invariants

- ADR-0001 — integer-microdollar money (no floats ever touch value).
- ADR-0002 — success-only billing (meter on generation success, never on attempt).
- Constant-time `X-API-Key` auth (`subtle`), fail-closed env config.
- Standalone workspace excluded from the root desktop GPUI/Metal build.
- All sibling HTTP behind injectable traits, faked in `passport-testkit`.

---

## 7. § Scale reality — grounded against mint

Estimates here are anchored to a **measured** sibling, not guessed. `apps/mint`
is the closest analog (money infra ↔ identity infra, same house rules, same
standalone-workspace discipline).

### 7.1 The measured baselines

| workspace | crates | `.rs` files | LOC | status |
|---|---:|---:|---:|---|
| **mint** (measured today) | 10 | 108 | ~23,025 | mature sibling |
| **passport Sx0** (measured today) | 9 | 48 | ~3,731 | shipped slice |

Passport Sx0 is **~16% of mint's LOC** in **90% of mint's crate count** — i.e. the
crates exist but are *thin*. That gap is the honest measure of how much remains.

### 7.2 Enterprise per-crate estimate

Projected from mint's density (~213 LOC/file, ~2,300 LOC/crate) adjusted per
crate's complexity (the moat and the server are denser; clients and SDKs are
lighter):

| # | crate | est. modules | est. files | est. LOC |
|---|---|---:|---:|---:|
| 1 | passport-core | 10 | 16 | 3,000 |
| 2 | passport-assets | 9 | 12 | 2,400 |
| 3 | passport-rights | 6 | 9 | 1,600 |
| 4 | passport-protocol | 6 | 9 | 1,500 |
| 5 | passport-resolve | 10 | 14 | 2,600 |
| 6 | passport-providers | 7 | 20 | 3,800 |
| 7 | **passport-consistency (moat)** | 12 | 22 | 4,400 |
| 8 | passport-artifacts | 9 | 14 | 2,800 |
| 9 | passport-provenance | 8 | 11 | 2,200 |
| 10 | passport-watermark | 6 | 7 | 1,200 |
| 11 | passport-personalize | 6 | 9 | 1,500 |
| 12 | passport-audit | 5 | 7 | 1,200 |
| 13 | passport-store | 3 | 5 | 800 |
| 14 | passport-store-postgres | 8 | 12 | 2,400 |
| 15 | passport-relay-client | 4 | 6 | 1,000 |
| 16 | passport-mint-client | 4 | 6 | 950 |
| 17 | passport-ledgerline-client | 3 | 5 | 900 |
| 18 | passport-server | 16 | 24 | 4,600 |
| 19 | passport-sdk-rust | 5 | 7 | 1,100 |
| 20 | passport-sdk-python | 6 | 6 | 900 |
| 21 | passport-sdk-ts | 7 | 8 | 1,300 |
| 22 | passport-testkit | 8 | 8 | 1,300 |
| | **TOTAL** | **~168** | **~237** | **~42,450** |

### 7.3 The headline

> **Enterprise Passport ≈ 22 crates / ~237 files / ~42.5K LOC ≈ 1.8–2× mint.**

That multiple is expected and *correct*: Passport carries everything mint carries
(a kernel, stores, a server, protocol, clients, settlement) **plus** two heavy
subsystems mint has no analog for — the **consistency moat** (scoring/drift/eval,
~4.4K LOC) and the **artifact training lifecycle** (~2.8K LOC) — plus provenance
signing, watermarking, personalization, and three language SDKs.

### 7.4 The done ladder — a slice is not the platform

The single most important number above is the gap between **~3.7K LOC shipped**
and **~42.5K LOC target**: today's real, tested Sx0 is roughly **9% of the
enterprise platform by volume**. The ladder below is the explicit rebuttal to
calling any thin slice "done":

| rung | scope | crates real | ~LOC | "done" means |
|---|---|---:|---:|---|
| **Sx0** ✅ | core loop: create→lock→version→resolve→generate(fake Relay/Mint)→provenance shape; in-mem + PG; auth | **9 (thin)** | ~3.7K | the **spine** compiles and is tested — *not* the platform |
| **Sx1** | live Relay + Mint wiring; `provider_artifacts` map; AITP-402 royalty transfer; Ledgerline emit | 12 | ~9K | real cross-service money + fan-out |
| **Sx2** | `passport-providers` (5 families); `passport-assets` split; grants + access control; **TS SDK** | 16 | ~18K | Menivor can generate-AS in production |
| **Sx3** | **`passport-consistency` (the moat)**: face/voice/style scoring, drift, seed bank, eval harness, regenerate policy | 18 | ~27K | consistency is *measured and enforced*, not hoped |
| **Sx4** | `passport-artifacts` training/broker; `passport-provenance` Ed25519 + Merkle + real C2PA JUMBF; `passport-watermark` | 20 | ~35K | Family-B gaps get *filled*; outputs are *credentialed + traceable* |
| **Sx5** | `passport-personalize`; `passport-audit` hash-chain; Rust + Python SDKs; full Vend/Turnstile/Abacus/agent-runtime surfaces; royalty marketplace | **22** | **~42.5K** | the enterprise platform |

**Explicit rebuttal:** the shipped 9 crates are a genuine, honest spine — but they
are Sx0. Anyone pointing at 3.7K LOC of create/lock/resolve and calling "Passport"
finished is mislabeling **~9%** of the platform as **100%**. The moat
(`passport-consistency`) — the entire reason Passport is defensible — does not
exist until **Sx3**. Provenance is a *manifest shape*, not a signed credential,
until **Sx4**. "Done" is Sx5.

---

## 8. § Owner decisions

These are the load-bearing product/architecture choices the owner must make; each
changes crate scope, legal posture, or economics. Recommendations are given but
the decision is the owner's.

### 8.1 Consistency bar — best-effort vs verified+regenerate
- **Option A (best-effort):** resolve and generate; report a score but ship
  whatever comes back. Cheap; no `passport-consistency` policy loop.
- **Option B (verified + regenerate):** enforce a per-passport bar; below-bar
  candidates regenerate (seed bank) or reject. This is the diamond's merge step.
- **Recommendation:** **B**, per-passport-configurable, with a global floor.
  Verified consistency *is* the moat; best-effort commoditizes Passport into a
  struct store. Ship A only as a temporary Sx2 fallback before Sx3 lands.

### 8.2 Trained artifacts — build vs broker vs reference-only
- **Reference-only:** never train; only Family-A providers get high consistency;
  Family-B providers always return `needs_artifact`. Lowest cost/legal exposure.
- **Broker:** orchestrate third-party training (ElevenLabs voice clone, a LoRA
  host); Passport holds the token, not the training pipeline.
- **Build:** run training in-house (`passport-artifacts` training modules).
- **Recommendation:** **broker first, build later.** Broker at Sx4 to satisfy
  `needs_artifact` without owning GPU training; revisit build only if a provider
  gap or margin justifies it. Reference-only is not enough for video/voice.

### 8.3 Rights / deepfake posture
- Consent-to-likeness is **mandatory** to lock a character carrying a real
  person's likeness; `passport-rights` gates `lock` and `generate-AS`.
- **Recommendation:** default-deny deepfakes of real people without a recorded
  `LikenessGrant`; require `DisclosureLevel` on any human-likeness output;
  rights-grant is always behind the human gate (§5.2). Log every rights decision
  to the audit spine. Treat this as legal infra, not a checkbox.

### 8.4 C2PA depth
- **Shape-only** (today): a manifest struct, unsigned.
- **Signed:** Ed25519 over the canonical manifest, digest hard-bound to the
  locked version.
- **Anchored + JUMBF:** Merkle-anchored batches + a real C2PA JUMBF export
  embeddable in the media, plus watermark for survivability.
- **Recommendation:** **signed at Sx4, JUMBF + watermark at Sx4/Sx5.** Provenance
  that can't be verified externally isn't provenance. Pair C2PA (survives if
  metadata is kept) with `passport-watermark` (survives screenshotting).

### 8.5 Royalty economics
- Whether/what to charge when a third party generates AS an owner's passport;
  the AITP-402 transfer amount and split.
- **Recommendation:** owner-set per-passport royalty (default 0), settled
  success-only through Mint on the same `mint_receipt_ref`; make it opt-in so
  free internal use (Menivor on first-party passports) costs nothing, while a
  creator marketplace (Sx5) can turn it on. Keep the identity↔money join
  (§6.3) authoritative — royalties are a projection of it, never a side ledger.

### 8.6 SDK priority — TS-first for Menivor
- Three SDKs are planned (Rust, Python, TS).
- **Recommendation:** **TS-first.** Menivor is the first heavy consumer and is
  TypeScript; `passport-sdk-ts` ships at **Sx2** (unblocks production generate-AS),
  Rust SDK at Sx5 (agent-runtime/Abacus embedding), Python SDK at Sx5 (training/
  eval authoring). Do not gold-plate Rust/Python SDKs before a real consumer needs
  them — build the SDK the first payer actually calls.

---

## Appendix A — glossary

- **Family A / Family B** — portable inference-time reference (A: reference images,
  persona text) vs non-portable per-provider trained artifact (B: LoRA, avatar,
  every voice clone). The `IdentityMechanism` enum in `passport-resolve`.
- **`needs_artifact`** — typed resolve outcome: a Family-B provider lacks its minted
  artifact; the resolver refuses to degrade silently.
- **`resolve_unavailable`** — typed resolve outcome: a provider has no identity path
  for the modality / the payload would be empty.
- **Locked version** — an immutable, content-addressed (`ni:///sha-256;…`) snapshot;
  the unit of value.
- **`generate AS passport`** — the diamond task (§5) that produces an on-identity
  output on a target provider.
- **`mint_receipt_ref`** — the identity↔money join key (§6.3, edge R22).
- **The moat** — `passport-consistency`: measured, enforced cross-provider
  consistency (face/voice/style scoring, drift, seed bank, eval, regenerate).

## Appendix B — the done ladder, restated

**Sx0 (shipped, ~9%) is a spine, not a platform.** 9 thin crates, ~3.7K LOC, 26
tests. The consistency moat starts at Sx3; verifiable provenance at Sx4; the
enterprise target (22 crates, ~237 files, ~42.5K LOC, ~2× mint) is Sx5. Do not
mislabel the spine as the finished product.
