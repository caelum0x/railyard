# Railyard

**The AI settlement + model rail.** Railyard is the platform infrastructure extracted from the Menivor (videogen) monorepo so the rail can serve *many* products — Menivor, and future payers — rather than being coupled to one.

## Thesis
The durable asset is the **settlement rail underneath every app**: **Mint** (a fiat-backed, gasless, success-only microdollar ledger with hash-linked receipts, AITP-402). Every other app is either the model rail (**Relay**) or a wedge that installs Mint under a new class of payer.

## Apps
| App | Role | Status |
|---|---|---|
| **mint** | Settlement ledger — double-entry, Ed25519 receipts, reserve conservation. The core asset. | ship-ready core |
| **relay** | Model gateway (TensorZero fork) + success-only Mint billing. The model rail. | needs price catalog |
| **agent-runtime** | Shared AITP-402 payment SDK substrate (ed25519 signing, paid-inference decorator). | payment SDK ready |
| **vend** | Stripe-meets-OpenRouter: resellable metered AI SDK on the rail. | needs crash reaper |
| **abacus** | Inline proxy enforcing hard, receipted per-agent spend budgets (kill-switch). | single-process |
| **turnstile** | Drop-in agent paywall middleware ("Stripe for AI agents"). | engine solid, SDK thin |
| **context-passport** | **Character Lock Passport** — provider-agnostic locked character/brand/personalization identity, resolved to any AI provider via Relay for consistent generation. | vision (see apps/context-passport/VISION.md) |
| **ledgerline** | Gross-margin lens: revenue + provider COGS on one Mint-backed ledger. | non-atomic settle |

## Consumed by
Menivor (`../videogen`) and other products call Railyard over HTTP + shared contract packages — they do **not** own it.

## Shared workstreams (from evaluation)
1. **Shared transactional datastore** — abacus/turnstile/vend keep holds/nonces/receipts in-process; horizontal scale needs a shared store + a hold-reaper for panic-orphaned holds.
2. **Per-model price catalog** — relay bills every model at one flat default; needs real per-model pricing.
3. **Ledgerline atomicity** — the hold+capture+COGS trio needs a durable outbox/compensation.
