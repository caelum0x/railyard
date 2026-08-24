# Context Passport — Character Lock Passport

**Status: runnable Sx0 product slice (multi-crate Rust workspace).** A
provider-agnostic, versioned, **lockable** identity for an AI character / brand /
persona that resolves into per-provider conditioning so the *same* identity stays
consistent across every model provider on the Railyard rail — and settles like
any other rail spend.

The full thesis is in [`VISION.md`](./VISION.md); the architecture, crate split,
and rail-integration plan are in [`ARCHITECTURE.md`](./ARCHITECTURE.md).
`PLAN.md` is the superseded OAuth-identity concept, kept for history.

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
| `passport-core` | domain kernel, no I/O: `Passport`/`PassportVersion`, typed `PassportAssets` (character/brand/persona) + patches, `Rights` (consent/likeness/disclosure), `VersionDigest` content-addressing, and the injectable `PassportStore` / `RelayConditioningSink` / `MintMeter` ports + `PassportError` |
| `passport-resolve` | the resolver: `Modality`, `ConditioningPayload`, a capability-routed `ProviderRegistry` of per-modality `ProviderAdapter`s (image/voice/video/text), fail-closed with `needs_artifact` for non-portable providers |
| `passport-provenance` | C2PA-aligned `GenerationRecord` (frozen rights snapshot), a `ProvenanceLog` port + in-memory impl, and a `to_c2pa_manifest` exporter |
| `passport-store` | `MemoryStore` implementing both persistence ports (tests / embedded demos) |
| `passport-store-postgres` | Neon/Postgres `PostgresStore` via the sqlx runtime API (compiles with no `DATABASE_URL`) + migrations |
| `passport-relay-client` | real reqwest `RelayConditioningSink` — submits resolved conditioning to Relay |
| `passport-mint-client` | real reqwest `MintMeter` — settles passport-conditioned generations on Mint |
| `passport-server` | axum API + `PassportService` wiring store + provenance + Relay + Mint from env |
| `passport-testkit` | `FakeRelay` / `FakeMint` transport fakes for tests |

## The core loop (Sx0)

`create → edit (draft) → lock (immutable + content-addressed) → version (fork
from a locked one) → resolve(provider, modality) → generate AS passport
(Relay + provenance + Mint) → audit`.

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
| `GET /v1/passports` | `X-API-Key` | List; optional `owner`, `kind`, `limit` |
| `GET /v1/passports/:id` | `X-API-Key` | Fetch a passport + head version |
| `PATCH /v1/passports/:id` | `X-API-Key` | Merge a typed asset patch into the head **draft** (locked → `409`) |
| `POST /v1/passports/:id/lock` | `X-API-Key` | Lock the head version (validates rights, content-addresses, immutable) |
| `POST /v1/passports/:id/versions` | `X-API-Key` | Branch a new draft from the locked head |
| `GET /v1/passports/:id/versions/:v` | `X-API-Key` | Fetch a specific version |
| `POST /v1/passports/:id/resolve` | `X-API-Key` | Resolve a locked version → provider `ConditioningPayload` |
| `POST /v1/passports/:id/generate` | `X-API-Key` | Generate AS this passport via Relay; record provenance; meter Mint |
| `GET /v1/passports/:id/generations` | `X-API-Key` | Provenance: assets conditioned by this passport |

## Run locally

```bash
cd apps/context-passport
DATABASE_URL='postgres://postgres:postgres@localhost:5432/passport' \
PASSPORT_API_KEY='passport-client-key' \
RELAY_BASE='http://127.0.0.1:3000' RELAY_API_KEY='relay-secret' \
MINT_BASE='http://127.0.0.1:8080'  MINT_API_KEY='mint-secret' \
cargo run -p passport-server
```

The server runs its embedded migration at startup and listens on `0.0.0.0:8095`
by default (`PASSPORT_BIND_ADDR`). No deployment starts without its database or
credentials.

### Environment contract

| Variable | Required | Meaning |
|---|---:|---|
| `DATABASE_URL` | yes | Postgres DSN for passport metadata, versions, provenance |
| `PASSPORT_API_KEY` | yes | Credential clients send as `X-API-Key` |
| `RELAY_BASE` / `RELAY_API_KEY` | yes | Relay gateway base URL + key |
| `MINT_BASE` / `MINT_API_KEY` | yes | Mint settlement base URL + key |
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
and the Relay/Mint seams are real and tested with fakes. Deferred to Sx1+ (see
`ARCHITECTURE.md`): the live Relay conditioning endpoint contract and the Mint
metering endpoint; the per-provider `provider_artifacts` minting/registry
subsystem; Ed25519 signing + Merkle anchoring of locked versions and a real C2PA
JUMBF exporter; `passport_grants` cross-product access control; and the Vend /
Turnstile / Abacus / agent-runtime distribution surfaces.
