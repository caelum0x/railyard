# Character Lock Passport — Architecture & Directory Plan

> Authoritative build plan. Supersedes `PLAN.md` (the old OAuth-identity concept).
> Implements `VISION.md`: a provider-agnostic, versioned, **lockable** identity
> for an AI character / brand / persona that resolves into per-provider
> conditioning so the *same* identity stays consistent across every model
> provider on the Railyard rail, and settles like any other rail spend.

## 1. What this service is (and is not)

The Passport is the **durable, reusable identity asset** — like Mint is the
durable money asset. It is:

- a **typed, versioned record** of what makes a character/brand/persona itself
  (face refs, LoRA/embedding, voice id, avatar, wardrobe, style, personality;
  brand logo/palette/typography/watermark/voice-tone; persona audience/language/
  platform/hooks), plus **rights/consent** and an **integrity digest**;
- **lockable**: locking a version freezes it, content-addresses it, and makes it
  immutable forever; new edits fork a new draft version from a locked one;
- a **resolver**: `resolve(passport_id, provider, modality) → ConditioningPayload`
  turns the locked identity into the exact conditioning a given provider accepts;
- **provenance-bearing**: every generated asset records which passport + which
  locked version + which provider conditioned it (C2PA-aligned).

It is **not** the inference gateway (that is Relay), **not** the money ledger
(that is Mint), and **not** a per-product feature (it is shared rail infra).

## 2. Boundary with the rest of the rail

```
             ┌──────────────────────────── Railyard rail ────────────────────────────┐
  edit/lock  │                                                                        │
  a passport │   context-passport ──resolve──► ConditioningPayload                    │
  ──────────►│        │  (identity supplier)          │                              │
             │        │                               ▼                              │
             │        │                        Relay (provider fan-out) ──► provider  │
             │        │                               │  image/video/voice/text       │
             │        │  provenance◄──────────────────┘                              │
             │        │                               │                              │
             │        └── meter (passport_id, version)─┼──► Mint (settlement, AITP-402)│
             │                                         └──► Ledgerline (margin lens)   │
             └────────────────────────────────────────────────────────────────────────┘
```

The Passport **supplies conditioning**; Relay **executes**; Mint **settles**;
Ledgerline **measures**; Vend / Turnstile / Abacus / agent-runtime are
**distribution & enforcement surfaces** that ride the same rail.

## 3. Directory / crate plan

Standalone cargo workspace (empty `[workspace]`, excluded from the root
GPUI/Metal graph — exactly like `apps/mint`, `apps/vend`, `apps/abacus`). Layout
mirrors `vend`'s crate discipline (domain kernel → ports → adapters → server →
testkit). Dependency direction is strictly downward; nothing depends on
`passport-server`.

```
apps/context-passport/
├── Cargo.toml                      # [workspace] members + [workspace.dependencies]
├── ARCHITECTURE.md                 # this file (source of truth)
├── VISION.md                       # product thesis
├── README.md                       # run/validate/env contract
├── crates/
│   ├── passport-core/              # domain kernel, NO I/O
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── ids.rs              # PassportId / VersionId / GenerationId newtypes
│   │   │   ├── kind.rs            # PassportKind: character | brand | persona
│   │   │   ├── assets/            # typed, versioned identity fields
│   │   │   │   ├── mod.rs         # PassportAssets enum + AssetsPatch + merge
│   │   │   │   ├── character.rs   # face_refs, voice_id, avatar_ref, lora_ref, style, personality, wardrobe
│   │   │   │   ├── brand.rs       # logo, palette, typography, watermark, voice_tone
│   │   │   │   └── persona.rs     # audience, language, platform, hooks
│   │   │   ├── rights.rs          # consent-to-likeness, disclosure, license terms
│   │   │   ├── version.rs         # PassportVersion, VersionStatus, lock invariants
│   │   │   ├── passport.rs        # Passport aggregate + CreatePassport + PassportView
│   │   │   ├── digest.rs          # canonical serialization + content hash (lock integrity)
│   │   │   ├── ports.rs           # PassportStore, RelayConditioningSink, MintMeter traits
│   │   │   └── error.rs           # PassportError + HTTP-agnostic envelopes
│   │   └── Cargo.toml
│   │
│   ├── passport-resolve/           # THE novel core: identity → provider conditioning
│   │   ├── src/
│   │   │   ├── lib.rs
│   │   │   ├── modality.rs        # Image | Voice | Video | Text
│   │   │   ├── payload.rs         # ConditioningPayload (provider-neutral)
│   │   │   ├── capability.rs      # what identity inputs a provider accepts
│   │   │   ├── registry.rs        # ProviderRegistry: (provider, modality) → adapter
│   │   │   ├── adapter.rs         # ProviderAdapter trait + fallback/negotiation
│   │   │   └── adapters/          # concrete mappings (real, not stubs)
│   │   │       ├── image.rs       # face_refs + lora/embedding + style prompt
│   │   │       ├── voice.rs       # voice_id (+ tone directives)
│   │   │       ├── video.rs       # avatar_ref + face_refs + voice_id
│   │   │       └── text.rs        # persona/voice-tone/hooks directives
│   │   └── Cargo.toml
│   │
│   ├── passport-provenance/        # C2PA-aligned generated-asset lineage
│   │   ├── src/{lib.rs, record.rs, manifest.rs, audit.rs}
│   │   └── Cargo.toml
│   │
│   ├── passport-store/             # PassportStore port + InMemoryStore
│   │   ├── src/{lib.rs, memory.rs}
│   │   └── Cargo.toml
│   │
│   ├── passport-store-postgres/    # Neon Postgres impl + migrations (sqlx runtime API)
│   │   ├── migrations/0001_passports.sql
│   │   ├── src/{lib.rs, store.rs, rows.rs, migrate.rs}
│   │   └── Cargo.toml
│   │
│   ├── passport-relay-client/      # real reqwest RelayConditioningSink (faked in tests)
│   │   ├── src/{lib.rs, client.rs}
│   │   └── Cargo.toml
│   │
│   ├── passport-mint-client/       # real reqwest MintMeter (faked in tests)
│   │   ├── src/{lib.rs, client.rs}
│   │   └── Cargo.toml
│   │
│   ├── passport-server/            # axum data+control plane binary
│   │   ├── src/
│   │   │   ├── main.rs            # env config, fail-closed wiring
│   │   │   ├── lib.rs
│   │   │   ├── state.rs           # AppState (service + clients + api_key)
│   │   │   ├── auth.rs            # constant-time X-API-Key (subtle)
│   │   │   ├── router.rs
│   │   │   ├── dto.rs
│   │   │   ├── service.rs         # business logic: create→edit→lock→version→resolve→generate
│   │   │   └── handlers/{passports.rs, versions.rs, resolve.rs, generate.rs, health.rs}
│   │   └── Cargo.toml
│   │
│   └── passport-testkit/           # FakeRelay / FakeMint / InMemoryStore fixtures
│       ├── src/lib.rs
│       └── Cargo.toml
```

Crate responsibilities and public surface:

| crate | role | key public types | depends on |
|---|---|---|---|
| `passport-core` | domain kernel, no I/O | `Passport`, `PassportVersion`, `PassportAssets`/`AssetsPatch`, `Rights`, `VersionDigest`, `PassportStore`/`RelayConditioningSink`/`MintMeter` ports, `PassportError` | — |
| `passport-resolve` | identity→conditioning | `Modality`, `ConditioningPayload`, `ProviderAdapter`, `ProviderRegistry`, `resolve()` | core |
| `passport-provenance` | asset lineage (C2PA) | `GenerationRecord`, `ProvenanceManifest`, `AuditEvent` | core |
| `passport-store` | port + in-memory | `InMemoryStore` | core |
| `passport-store-postgres` | Neon impl + migrations | `PostgresStore` | core |
| `passport-relay-client` | submit conditioning to Relay | `RelayClient` | core, resolve |
| `passport-mint-client` | meter/settle on Mint | `MintClient` | core |
| `passport-server` | axum binary | `AppState`, `build_router`, `PassportService` | all above |
| `passport-testkit` | shared fakes/fixtures | `FakeRelay`, `FakeMint`, fixtures | core, store, resolve |

## 4. Data model (Postgres source of truth; Mint stays the money ledger)

- `passports` — id, kind, owner, status(draft|locked), head version, timestamps.
- `passport_versions` — (passport_id, version) PK, status, **assets JSONB**,
  **rights JSONB**, **content_digest** (hex hash of canonical serialization, set
  at lock), created/updated/locked_at. Locked rows are never rewritten.
- `passport_idempotency` — idempotency_key → passport_id (safe create retries).
- `provider_artifacts` — (passport_id, version, provider, modality) → trained
  artifact ref (LoRA/embedding/voice id). Some providers *cannot* share a raw
  reference and need a per-provider trained artifact; the resolver reads this map.
- `passport_grants` — who may `generate AS` a passport (cross-product access +
  creator licensing), with scope + expiry.
- `passport_generations` — provenance: id, passport_id, version, content_digest,
  provider, modality, output_ref, C2PA assertions, mint_receipt_ref, created_at.

## 5. Resolve contract (the hard, novel core)

`resolve(version: &PassportVersion, provider, modality) → ConditioningPayload`,
**fail-closed**: an unlocked version never resolves; a modality with no usable
conditioning returns `resolve_unavailable` rather than an empty payload.

- `ProviderRegistry` maps `(provider, modality) → ProviderAdapter`. A generic
  per-modality adapter is the default; named adapters (runway/kling/veo,
  elevenlabs/cartesia, heygen/hedra, gpt-image/flux/imagen) refine it.
- `ConditioningPayload` is provider-neutral: `reference_images[]`, `lora_ref`,
  `embedding_ref`, `avatar_ref`, `voice_id`, `style_prompt`, `negative_prompt`,
  `seed`, `text_directives[]`, `extra{}` (palette, platform, language …).
- **Trained-artifact constraint** (from provider research): providers differ on
  whether identity is a *reference* (portable: face image, voice id) or a
  *trained artifact* (non-portable: a LoRA/DreamBooth/fine-tune that must be
  trained per provider). The resolver consults `provider_artifacts` and, when a
  provider needs an artifact it does not yet have, returns a typed
  `needs_artifact` signal (extension point for an async training pipeline) rather
  than silently degrading consistency.

## 6. Rail integration — how it works with every Railyard app

| App | Relationship | Concrete seam |
|---|---|---|
| **Relay** | executor | `passport-relay-client` attaches a resolved `ConditioningPayload` to a Relay inference request (`generate AS passport X`). Relay fans out to the actual provider (Veo/Runway/Kling/HeyGen/ElevenLabs/LoRA). |
| **Mint** | settlement | `passport-mint-client` meters each passport-conditioned generation success-only (ADR-0002), tagging the Mint receipt with `passport_id`/`version`. Optional **creator licensing fee**: an AITP-402 transfer to the passport owner's Mint account when a third party generates AS their passport. |
| **Ledgerline** | margin lens | Each generation emits a Ledgerline margin event tagged `passport_id`/`version`/`kind` → per-character / per-brand gross-margin analytics. |
| **Vend** | white-label distribution | Vend tenants resolve a passport (HTTP) before calling Relay, so resold, metered inference stays on-identity. Passport is the shared identity layer under Vend's multitenancy. |
| **Turnstile** | paid tool gate | `generate AS passport X` is exposed as a paid AITP-402 tool; Turnstile gates it and settles the licensing fee on Mint. |
| **Abacus** | budget enforcement | Passport-conditioned generations flow through Abacus so an agent embodying a character has a hard, receipted spend budget. |
| **agent-runtime** | embodiment | An agent "wears" a passport: the resolved persona/voice/text conditioning is loaded into the rig `Agent` so it speaks and acts as the locked character, paying via the AITP-402 seam. |

Reused invariants: ADR-0001 (integer-microdollar money), ADR-0002 (success-only
billing), constant-time `X-API-Key` auth, fail-closed env config, standalone
workspace excluded from the root desktop build.

## 7. Phased roadmap (Sx0 → Sx4)

- **Sx0 (this slice)** — core loop `create → edit(draft) → lock(immutable,
  content-digested) → version(fork from locked) → resolve(image|voice|video|text)
  → record provenance`. In-memory + Postgres stores behind a trait; axum server;
  constant-time auth; idempotent create. Real, tested Rust; no `todo!()`.
- **Sx1** — Relay wiring (`generate AS passport`), Mint metering + creator
  licensing transfer, provider adapter registry + `provider_artifacts` map.
- **Sx2** — Vend / Turnstile / Abacus distribution surfaces; `passport_grants`
  access control; client SDK.
- **Sx3** — consistency eval harness (does the output actually match the locked
  identity? face/voice similarity scoring) + C2PA signing of outputs.
- **Sx4** — creator marketplace of passports + agent-runtime embodiment +
  cross-product royalty settlement.

## 8. Sx0 status

The Sx0 core loop is implemented and tested (create→lock→immutability,
versioning/fork, resolve per modality, auth rejection). This document plans the
crate split it is being refactored into; the integration crates
(`passport-relay-client`, `passport-mint-client`, `passport-provenance` signing)
carry real port traits with test fakes now, and their live HTTP wiring is the
Sx1 deliverable, tracked above as explicit extension points.
