# Railyard

**The AI settlement + model rail.** Railyard is the platform infrastructure extracted from the Menivor (videogen) monorepo so the rail can serve *many* products — Menivor, and future payers — rather than being coupled to one.

## Thesis
The durable asset is the **settlement rail underneath every app**: **Mint** (a fiat-backed, gasless, success-only microdollar ledger with hash-linked receipts, AITP-402). Every other app is either the model rail (**Relay**) or a wedge that installs Mint under a new class of payer.

## Apps
| App | Role | Status |
|---|---|---|
| **mint** | Settlement ledger — double-entry, Ed25519 receipts, reserve conservation. The core asset. | ship-ready core |
| **relay** | Model gateway (TensorZero fork) + success-only Mint billing. The model rail. | needs price catalog |
| **agent-runtime** | Shared Rig/AITP-402 runtime: policy signing, paid SSE/multipart, cancellation reconciliation, pinned Mint-proof verification. | payment SDK ready |
| **vend** | Stripe-meets-OpenRouter: resellable metered AI SDK on the rail. | needs crash reaper |
| **abacus** | Inline proxy enforcing hard, receipted per-agent spend budgets (kill-switch). | single-process |
| **turnstile** | Drop-in agent paywall middleware ("Stripe for AI agents"). | durable paywall core + Axum adapter |
| **identity-lock** | Provider-agnostic locked character/brand/persona identity, resolved through Relay for consistent generation. Legacy source path: `apps/context-passport`. | runnable Sx0 |
| **ledgerline** | Gross-margin lens: revenue + provider COGS on one Mint-backed ledger. | non-atomic settle |

## Consumed by
Menivor (`../videogen`) and other products call Railyard over HTTP + shared contract packages — they do **not** own it.

## Shared workstreams (from evaluation)
1. **Shared transactional datastore** — Turnstile and Abacus now ship Postgres ownership + leased recovery; remaining datastore work is concentrated in the apps still using local/file state.
2. **Per-model price catalog** — relay bills every model at one flat default; needs real per-model pricing.
3. **Ledgerline atomicity** — the hold+capture+COGS trio needs a durable outbox/compensation.
