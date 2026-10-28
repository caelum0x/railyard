# Identity Lock — Versioned AI Character and Brand Identity

## Deployed (2026-10-28)

Live on railyard-box (`130.61.113.116`), `profile: context-passport` in
`infra-box/stacks/railyard/docker-compose.yml` (new profile this pass — this
app had no Dockerfile and no deploy scaffolding at all). Added a standalone
distroless `Dockerfile` + `Dockerfile.dockerignore`: this workspace
path-depends on `apps/mint/crates/mint-protocol` (and its siblings
`mint-core`/`mint-store`/`mint-receipts`/`mint-ledger`), outside its own
workspace, so — like `apps/turnstile`'s Dockerfile — it builds from the repo
root with the context trimmed to exactly those crates plus this app's own.
Container port 8095 -> host `127.0.0.1:4400` -> Caddy
`context-passport.{$RAILYARD_DOMAIN}`. Own Postgres database
`context_passport` (role `context_passport_app`). Wired to the real,
already-live Mint via well-known key discovery (`MINT_BASE_URL=http://
mint:8080` -> `GET /.well-known/mint-verification-keys.json`, confirmed in
the boot log: `mint receipt keys discovered`) and the real Relay (`RELAY_BASE
=http://relay:3000`, a real `sk-t0-` key minted with `gateway
--create-api-key`).

**Found and fixed a real bug this way**, the same bar as the Toolgate
catalog-page and Turnstile bech32 bugs from earlier sessions: `crates/
passport-relay-client`'s `RelayClient::generate` sent `X-API-Key` to Relay's
`/v1/passport/generate`, but Relay's gateway only accepts `Authorization:
Bearer <key>` there — every real call failed `401 Unauthorized` before this
fix, invisible to every existing test because they all exercise
`build_inference_request`/`payment_evidence` directly and "tests fake the
sink via `FakeRelay`" (this README's own words), never the real `reqwest`
call. Fixed (one line) and added a real regression test,
`generate_sends_bearer_authorization_not_x_api_key`: a genuine local axum
server (no fakes) that asserts the actual outbound `Authorization` header
through a real `generate()` call. Full workspace suite: 59/59 pass (was
58/58 before the new test). `cargo clippy -p passport-relay-client
--all-targets -- -D warnings`: clean.

**Verified live end-to-end against the real deployed container**, the full
core loop: real `POST /v1/passports` (brand kind, a real palette +
voice_tone), `POST .../lock` (real content-addressed digest), `POST
.../resolve` (real per-provider text directives), `GET .../versions`, `GET
/v1/passports`, `/console` (200). The `generate` chain, after the auth fix,
now genuinely reaches Relay and runs a real inference: with
`provider:"groq"` (Relay's live, funded default) the call produces a real
LLM completion — confirmed directly against Relay with the exact same
request Context Passport sends — and then correctly fails closed with
`relay_unavailable: Relay response missing AITP-Receipt`, because
**Relay's own AITP billing is globally off in this environment**
(`RELAY_BILLING_ENABLED` unset; confirmed by calling Relay's
`/v1/passport/generate` directly and finding no `AITP-Receipt` header on an
otherwise-200 response) — the exact same funding-wall boundary every other
Railyard sub-app in this session hit, not a Context Passport bug, and
correctly refuses to fabricate provenance without real settlement evidence
(this app's own design intent, per `main.rs`: "it never fabricates
settlement evidence"). `provider:"openai"` separately surfaces a real,
pre-existing Relay gap (`Dynamic api key \`relay_openai_api_key\` is
missing`) and its OpenRouter failover is credit-exhausted — both real,
unrelated, already-existing Relay-side conditions, not newly introduced by
this deploy. Flipping Relay's global AITP billing on to get a fully settled
generation would affect every live Relay caller, not just this one app, so
it is out of scope for this pass (same owner-gated-scale decision as the
other sub-app funding walls).

One step remains, infra not code, same single blocker as every other
sub-app deployed this session: `context-passport.ereboslab.com`'s DNS A
record doesn't exist yet (the only Cloudflare credential available is a
read-only MCP connection). Caddy is already configured and reloaded;
routing confirmed correct (`Host: context-passport.ereboslab.com` -> real
`308` redirect).

**Status: runnable Sx0 product slice (multi-crate Rust workspace).** A
provider-agnostic, versioned, **lockable** identity for an AI character / brand /
persona that resolves into per-provider conditioning so the *same* identity stays
consistent across every model provider on the Railyard rail — and settles like
any other rail spend.

The full thesis is in [`VISION.md`](./VISION.md); the architecture, crate split,
and rail-integration plan are in [`ARCHITECTURE.md`](./ARCHITECTURE.md).
This product is **Identity Lock**, not Context Passport. `PLAN.md` describes the
separate, approved Context Passport concept for user-owned structured context,
field-level consent, and OAuth authorization-code + PKCE portability. Keeping
the plan beside this legacy directory does not make those OAuth capabilities
part of Identity Lock; they require a separately named service and threat-model
gate before implementation.

## Standalone workspace

`apps/context-passport` is its OWN cargo workspace, **excluded** from the repo
root (like `apps/mint` / `apps/vend` / `apps/abacus`) so it never enters the
desktop GPUI/Metal build graph. Build and test from here only:

```bash
cd apps/context-passport && cargo test      # never from the repo root
```

## Crate graph

Dependency direction is a strict DAG rooted at `passport-core`; nothing depends
on `passport-server`.

| crate | role |
|-------|------|
| `passport-core` | domain kernel, no I/O: `Passport`/`PassportVersion`, typed `PassportAssets` (character/brand/persona/voice/style) + patches, `TrainedArtifact` (per-provider voice clones, avatars, LoRAs), `Rights` (consent/likeness/disclosure), `VersionDigest` content-addressing, and the injectable `PassportStore` / `RelayConditioningSink` / `MintMeter` ports + `PassportError` |
| `passport-resolve` | the resolver: `Modality`, `ConditioningPayload`, a capability-routed `ProviderRegistry` of per-modality `ProviderAdapter`s (image/voice/video/text), fail-closed with `needs_artifact` for non-portable providers |
| `passport-provenance` | C2PA-aligned `GenerationRecord` (frozen rights snapshot), a `ProvenanceLog` port + in-memory impl, and a `to_c2pa_manifest` exporter |
| `passport-store` | `MemoryStore` implementing both persistence ports (tests / embedded demos) |
| `passport-store-postgres` | Neon/Postgres `PostgresStore` via the sqlx runtime API (compiles with no `DATABASE_URL`) + migrations |
| `passport-relay-client` | real reqwest `RelayConditioningSink` — submits conditioning and requires request-correlated signed Mint evidence on Relay's successful response |
| `passport-mint-client` | legacy standalone `MintMeter` adapter retained for compatibility; it is not used by the generation path |
| `passport-server` | axum API + `PassportService` wiring store + provenance + receipted Relay from env |
| `passport-testkit` | `FakeRelay` / `FakeMint` transport fakes for tests |

## The core loop (Sx0)

`create → edit (draft) → lock (immutable + content-addressed) → version (fork
from a locked one) → resolve(provider, modality) → generate AS passport
(Relay's single AITP settlement + signed provenance) → audit`.

Two facts drive the design (from the provider research):

1. **Identity splits into two families.** *Family A* is portable inference-time
   reference (reference images, persona text — resent each call). *Family B* is a
   non-portable per-provider trained artifact (LoRA, HeyGen avatar, **every**
   voice clone). The resolver routes a neutral passport into whichever family a
   target provider supports and returns `needs_artifact` when a Family-B provider
   lacks its minted token — rather than silently degrading consistency.
2. **Locking content-addresses the identity.** Locking canonicalizes the version
   body and stamps a `ni:///sha-256;…` digest; that digest is the version's
   identity and is frozen into every generation's provenance (C2PA hard-binding).

## API surface

| Route | Auth | Purpose |
|---|---|---|
| `GET /healthz` | public | Liveness |
| `GET /readyz` | public | Postgres readiness |
| `POST /v1/passports` | `X-API-Key` | Create a draft passport (idempotent on `idempotency_key`) |
| `GET /v1/passports` | `X-API-Key` | List; optional `owner`, `kind`, `limit`, `include_archived`, `archived_only` (archived passports are hidden by default) |
| `GET /v1/passports/:id` | `X-API-Key` | Fetch a passport + head version |
| `PATCH /v1/passports/:id` | `X-API-Key` | Merge a typed asset patch into the head **draft** (locked → `409`) |
| `POST /v1/passports/:id/lock` | `X-API-Key` | Lock the head version (validates rights, content-addresses, immutable) |
| `POST /v1/passports/:id/archive` | `X-API-Key` | Archive: hidden from the default list, read-only (edit/lock/branch/generate/attest answer `409 passport_archived`); versions, generations and manifests stay readable |
| `POST /v1/passports/:id/unarchive` | `X-API-Key` | Restore an archived passport |
| `POST /v1/passports/:id/versions` | `X-API-Key` | Branch a new draft from the locked head |
| `GET /v1/passports/:id/versions` | `X-API-Key` | Full version history, oldest first |
| `GET /v1/passports/:id/versions/:v` | `X-API-Key` | Fetch a specific version |
| `POST /v1/passports/:id/resolve` | `X-API-Key` | Resolve a locked version → provider `ConditioningPayload` |
| `POST /v1/passports/:id/generate` | `X-API-Key` | Generate via Relay from `{provider, modality, prompt}`; returns the produced text (`output_text`, hard-bound by `asset_hash`) plus provenance with signed settlement evidence |
| `POST /v1/passports/:id/attest` | `X-API-Key` | Record a generation made elsewhere; the Mint receipt signature is verified (Ed25519) and a receipt can back only one generation |
| `POST /v1/passports/:id/artifacts` | `X-API-Key` | Register a provider-minted artifact (`voice_clone` / `avatar` / `lora`) for a locked version: `{provider, kind, version?, artifact_ref?, provider_model?}`. Without `artifact_ref` it starts `pending` (training running); a newer active registration for the same version/provider/kind supersedes the old one. Resolve uses the ready artifact before failing with `needs_artifact` |
| `GET /v1/passports/:id/artifacts` | `X-API-Key` | List trained artifacts, newest first |
| `GET /v1/passports/:id/artifacts/:artifact_id` | `X-API-Key` | Fetch one artifact |
| `PATCH /v1/passports/:id/artifacts/:artifact_id` | `X-API-Key` | Complete (`ready` + `artifact_ref`) or fail (`failed` + `failure_reason`) a pending artifact, or rotate a ready artifact's ref |
| `DELETE /v1/passports/:id/artifacts/:artifact_id` | `X-API-Key` | Revoke an artifact (allowed on archived passports; the record is kept for provenance) |
| `GET /v1/passports/:id/generations` | `X-API-Key` | Provenance: assets conditioned by this passport |
| `GET /v1/passports/:id/generations/:gen/manifest` | `X-API-Key` | Export one generation's C2PA-aligned Content Credential manifest |
| `GET /console` | page (key entered in browser) | Web console: create/edit/lock/version/archive passports, view any version, preview resolved conditioning, generate against a chosen locked version, attest external generations, browse generations + manifests |

Access control: the operator key (`PASSPORT_API_KEY`) sees everything; tenant
keys from `PASSPORT_OWNER_KEYS` act only as their owner (creates are stamped with
that owner, lists are filtered, other owners' passports answer `404`).

Modalities: Relay ships `passport_text`. Image/voice/video generation needs a
`passport_<modality>` function wired to an asset provider in Relay's config;
until then generate those assets with the provider directly and record them via
`/attest`.

## Run locally

```bash
cd apps/context-passport
DATABASE_URL='postgres://postgres:postgres@localhost:5432/passport' \
PASSPORT_API_KEY='passport-client-key' \
RELAY_BASE='http://127.0.0.1:3000' RELAY_API_KEY='relay-secret' \
cargo run -p passport-server
```

The server runs its embedded migration at startup and listens on `0.0.0.0:8095`
by default (`PASSPORT_BIND_ADDR`). No deployment starts without its database or
credentials.

### Environment contract

| Variable | Required | Meaning |
|---|---:|---|
| `DATABASE_URL` | yes | Postgres DSN for passport metadata, versions, provenance |
| `PASSPORT_API_KEY` | yes | Operator credential clients send as `X-API-Key` |
| `PASSPORT_OWNER_KEYS` | no | Per-owner tenant keys, `owner:key,owner2:key2` (keys 24+ chars) |
| `MINT_RECEIPT_PUBLIC_KEYS` | yes* | Hex Ed25519 Mint receipt keys (comma separated) used to verify settlement evidence |
| `MINT_BASE_URL` | yes* | Alternative: discover the keys from Mint's `/.well-known/mint-verification-keys.json` |
| `RELAY_BASE` / `RELAY_API_KEY` | yes | Relay gateway base URL + key |
| `PASSPORT_BIND_ADDR` | no | Listener; default `0.0.0.0:8095` |
| `RUST_LOG` | no | Tracing filter; default `info` |

## Validate

```bash
cd apps/context-passport
cargo fmt --all -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

## Sx0 boundary (documented extension points)

The resolver contract, capability routing, versioning/lock integrity, provenance,
and the Relay settlement/provenance seam is real and tested with fakes. Deferred
to Sx1+ (see `ARCHITECTURE.md`): live provider-specific Relay conditioning; the
per-provider `provider_artifacts` minting/registry
subsystem; Ed25519 signing + Merkle anchoring of locked versions and a real C2PA
JUMBF exporter; `passport_grants` cross-product access control; and the Vend /
Turnstile / Abacus / agent-runtime distribution surfaces.
