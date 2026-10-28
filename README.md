<div align="center">
  <img src="assets/brand/logo.png" alt="Railyard" width="120" />

  # Railyard

  **The AI settlement + model rail.**

  [![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
  [![Insights](https://img.shields.io/badge/insights-live-brightgreen)](https://insights.ereboslab.com)
  [![Faultline](https://img.shields.io/badge/errors-live-brightgreen)](https://errors.ereboslab.com)
</div>

---

Railyard is the platform infrastructure extracted from the Menivor (videogen)
monorepo so the rail can serve **many** products — Menivor, and future payers
— rather than being coupled to one. It's a from-scratch, owned alternative to
stitching together Stripe + Lago + a bespoke ledger + Sentry + a web-analytics
SaaS for every new product: one settlement core, with every other app either
*consuming* it or *installing* it under a new class of payer.

## Thesis

The durable asset is the **settlement rail underneath every app**: **Mint**
— a fiat-backed, gasless, success-only microdollar ledger with Ed25519
hash-linked receipts and reserve conservation, speaking AITP-402. Everything
else in this repo is either:

- the **model rail** (**Relay** — a TensorZero fork with success-only Mint
  billing), or
- the **tool rail** (**Switchyard** — one key, a priced tool catalog, the
  same hold/capture discipline), or
- a **wedge** that installs Mint under a new class of payer (billing,
  paywalls, a game economy, a resellable AI SDK, a forge, analytics, error
  tracking).

If an app in this repo doesn't touch Mint and isn't infrastructure those apps
run on, it probably doesn't belong here — see
[`docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md`](docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md)
§0 for the design invariants every app is held to.

## Apps

| App | Role | Status |
|---|---|---|
| **Mint** (`apps/mint`) | Settlement ledger: double-entry, Ed25519 hash-linked receipts, reserve conservation, payouts and provider reconciliation. The core asset. | implemented; operator console at `/admin` |
| **Relay** (`apps/relay`) | Model gateway (TensorZero fork) with success-only Mint billing and a per-model price catalog. The model rail. | implemented; operator UI in `apps/relay/ui` |
| **Rago** (`apps/rago`) | Usage-based billing (Rust rewrite of Lago, API-compatible): metering, subscriptions, invoicing, payments, integrations. | implemented; admin at `/admin`, customer portal at `/portal/:token` |
| **Insights** (`apps/insights-edge`) | Cookieless, GDPR-friendly web analytics: email+domain signup, private per-site dashboard, session replay (paid tier), UTM/referrer/page-speed reporting. | **live** at `insights.ereboslab.com`, free up to 10,000 events/month/site |
| **Faultline** (GlitchTip, box 2) | Sentry-compatible browser/server error tracking for the portfolio. | **live** at `errors.ereboslab.com`, wired into Scribase, Menivor, Bidstage |
| **Vend** (`apps/vend`) | Resellable metered AI SDK on the rail (Stripe-meets-OpenRouter). Interrupted holds and payouts are replayed by `vend-reconcile`. | **live** at `vend.ereboslab.com`; tenant console at `/console`, operator admin at `/admin` |
| **Turnstile** (`apps/turnstile`) | Drop-in paywall for paid agent tools (AITP-402 and x402). | **live** at `turnstile.ereboslab.com`; admin at `/admin` |
| **Rithub** (`apps/gitea`, decided 2026-10-15) | Self-hostable git forge + CI: Gitea (MIT) branded as "Rithub", with the Rago/Mint billing bridge and a registered Actions runner (`act_runner`) so pushed `.gitea/workflows/*.yml` jobs actually execute. `apps/rithub` (from-scratch Rust forge on `rit`) is the earlier effort — kept, not deleted, documented as superseded for this slot in `apps/rithub/README.md` and `apps/gitea/README.md`. | **live** at `git.ereboslab.com`; CI runner deployed 2026-10-20 |
| **rit** (`apps/rit`) | Library-first Rust implementation of Git's object/authoring model, interop-tested against `git`. | library |
| **agent-runtime** | Shared Rig/AITP-402 runtime: policy signing, paid SSE/multipart, cancellation reconciliation, pinned Mint-proof verification. | payment SDK ready |
| **abacus** | Inline proxy enforcing hard, receipted per-agent spend budgets (kill-switch). | **live** at `abacus.ereboslab.com`; Postgres-backed |
| **identity-lock** (Context Passport) | Provider-agnostic locked character/brand/persona identity, resolved through Relay for consistent generation. Source path: `apps/context-passport`. | **live** at `context-passport.ereboslab.com` |
| **ledgerline** | Gross-margin lens: revenue + provider COGS on one Mint-backed ledger, settled through a durable outbox. | **live** at `ledgerline.ereboslab.com` |
| **game-economy** | Storefront for card packs, cosmetics and battle passes on Mint hold/capture. | runnable slice |
| **Toolgate** (`apps/toolgate`, aka Switchyard) | The tool rail next to Relay: one `sk-t0-` key reaches a schema-typed catalog (our products first, free/public APIs second, BYO keys third) with Mint hold/capture, Rago events, SSRF-safe egress, an MCP server and the `sy` CLI. | **live** at `toolgate.ereboslab.com` |

`apps/ranguard` (userspace integrity-monitor demo) and `apps/rithub-forge`
(an optional Forgejo-based deployment, separate from Rithub) are not part of
the rail — see **License** below for why `rithub-forge` is called out
separately.

## Architecture

Every app in this repo is held to the same design invariants (full detail in
[`docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md`](docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md)):

1. **Each app is its own standalone Cargo/package workspace**, excluded from
   any repo-root build graph so apps never entangle each other's builds.
2. **Money math is integer microdollars.** No floats in any money path,
   anywhere, ever.
3. **Success-only billing.** Failed work creates no Mint movement — you are
   never charged for an agent run that didn't produce a result.
4. **Mint is the only vault.** Every other app holds no money; it calls Mint
   over HTTP for hold/capture/release/transfer and treats Mint's receipts as
   the system of record. Local Postgres in other apps is metadata/projection
   only, never a second source of truth for balances.
5. **All external I/O is behind injectable transport traits** — fake in
   tests, real `reqwest` in production — which is what makes cross-app
   composition actually testable instead of aspirational.

`apps/mint` (10 crates) is the reference shape every other app's target tree
is decomposed in: `*-core` (pure domain, no I/O) → `*-store` +
`*-store-postgres` (ports + Postgres impl) → `*-protocol`/`*-contract` (wire
types) → clients → services → `*-server` (binary) → `*-sdk` (thin public
consumer crate) → `*-testkit`.

### Switchyard (tool rail)

`apps/toolgate` is a standalone cargo workspace
(`cd apps/toolgate && cargo test --workspace`; no root workspace member line
needed, same as Abacus and Rago). It serves `/v1/tools/*` (search by task,
describe with price, call, batch), `/mcp` (streamable HTTP, OAuth 2.1 with
PKCE, DCR, CIMD and the device flow for `sy login`), `/admin` and `llms.txt`.
Keys are Relay's own `sk-t0-` keys, so one key covers models and tools. Paid
calls hold on Mint before the upstream call and capture the exact cost;
failures release the hold. Every call is written to a hash-chained audit log
together with its Rago `tool_call` event in one transaction. See
`apps/toolgate/README.md`.

## Why build vs. fork

Railyard targets ≈300K+ LOC of *owned* platform code, and deliberately does
**not** write everything from scratch —
[`docs/OSS_FOUNDATIONS.md`](docs/OSS_FOUNDATIONS.md) is the anti-"reinvent
it" map: `rit` forks on `gitoxide` (saves ~100K LOC vs. a from-scratch Git),
`relay` forks TensorZero (Apache-2.0, the single largest reuse on the
platform — Railyard owns only a thin billing/AITP overlay), `rithub` takes
Gitea's domain model as a reference blueprint (not a Go fork — GPLv3 Forgejo
is explicitly flagged **avoid** for anything that needs to stay permissively
licensed), and `context-passport` builds on `c2pa-rs` + `ort` (ONNX) rather
than hand-rolling provenance signing and cross-provider similarity scoring.
License discipline is explicit per dependency in that doc — MIT/Apache-2.0
targeted everywhere reasonably possible, GPL/AGPL and non-commercial model
weights flagged as avoid-or-risk rather than glossed over.

## Quickstart

Each app is its own workspace — there is no single root `build`/`test` that
spans the whole monorepo by design (invariant #1 above). Pick an app and
follow its own README:

```bash
# Settlement ledger
cd apps/mint && cargo test --workspace

# Model gateway
cd apps/relay && cargo test --workspace

# Billing
cd apps/rago && cargo test --workspace

# Tool rail
cd apps/toolgate && cargo test --workspace
```

`apps/DEPLOY-SHARED-PLANE.md` covers the shared deploy plane: Rago, Relay,
Mint and Abacus co-deploy on one box behind Caddy.

## Consumed by

Menivor (`../videogen`) and other products call Railyard over HTTP + shared
contract packages — they do **not** own it. This is the point: one rail,
many payers, instead of re-deriving billing/ledger/analytics/error-tracking
per product.

## Shared workstreams

1. **Shared transactional datastore**: Turnstile and Abacus ship Postgres
   ownership + leased recovery; remaining datastore work is concentrated in
   the apps still using local/file state.
2. **Shared deploy plane**: Rago, Relay, Mint and Abacus co-deploy on one box
   behind Caddy; see `apps/DEPLOY-SHARED-PLANE.md`.

Relay prices each model at its own list price (111 rows as of 2026-09-24,
including the OpenRouter free tier and Groq's published prices). It can also
refresh those prices from OpenRouter's public model list on a timer
(`RELAY_CATALOG_REFRESH_URL=openrouter`). Operator price edits always take
precedence over the feed.

## Contributing

See [`CONTRIBUTING.md`](CONTRIBUTING.md). Short version: each app is its own
workspace with its own tests — run those before opening a PR, and keep the
design invariants above (integer microdollars, success-only billing, Mint as
the only vault, injectable transport traits) intact in anything that touches
money.

## License

Railyard is licensed under [Apache-2.0](LICENSE).

**One carve-out:** `apps/rithub-forge` vendors Forgejo, which is GPLv3 — it
ships with its own `LICENSE` file and that subtree is governed by it, not by
the root Apache-2.0 license. It's an optional, separate deployment path (not
part of the rail — see the Apps table above) specifically so this license
boundary stays clean rather than contaminating the rest of the platform.
Everything else in this repository is Railyard's own code or Apache-2.0/MIT
dependencies per [`docs/OSS_FOUNDATIONS.md`](docs/OSS_FOUNDATIONS.md).
