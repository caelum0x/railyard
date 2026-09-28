# Railyard

**The AI settlement + model rail.** Railyard is the platform infrastructure extracted from the Menivor (videogen) monorepo so the rail can serve *many* products — Menivor, and future payers — rather than being coupled to one.

## Thesis
The durable asset is the **settlement rail underneath every app**: **Mint** (a fiat-backed, gasless, success-only microdollar ledger with hash-linked receipts, AITP-402). Every other app is either the model rail (**Relay**) or a wedge that installs Mint under a new class of payer.

## Apps
| App | Role | Status |
|---|---|---|
| **Mint** (`apps/mint`) | Settlement ledger: double-entry, Ed25519 hash-linked receipts, reserve conservation, payouts and provider reconciliation. The core asset. | implemented; operator console at `/admin` |
| **Relay** (`apps/relay`) | Model gateway (TensorZero fork) with success-only Mint billing and a per-model price catalog. The model rail. | implemented; operator UI in `apps/relay/ui` |
| **Rago** (`apps/rago`) | Usage-based billing (Rust rewrite of Lago, API-compatible): metering, subscriptions, invoicing, payments, integrations. | implemented; admin at `/admin`, customer portal at `/portal/:token` |
| **Vend** (`apps/vend`) | Resellable metered AI SDK on the rail (Stripe-meets-OpenRouter). Interrupted holds and payouts are replayed by `vend-reconcile`. | implemented; tenant console at `/console`, operator admin at `/admin` |
| **Turnstile** (`apps/turnstile`) | Drop-in paywall for paid agent tools (AITP-402 and x402). | runnable SDK + reference server; admin at `/admin` |
| **Rithub** (`apps/rithub`) | Self-hostable git forge on `rit`: repos, orgs, issues, pull requests, reviews, releases, CI checks, audit. | implemented; server-rendered web UI at `/` |
| **rit** (`apps/rit`) | Library-first Rust implementation of Git's object/authoring model, interop-tested against `git`. | library |
| **agent-runtime** | Shared Rig/AITP-402 runtime: policy signing, paid SSE/multipart, cancellation reconciliation, pinned Mint-proof verification. | payment SDK ready |
| **abacus** | Inline proxy enforcing hard, receipted per-agent spend budgets (kill-switch). | implemented; Postgres-backed |
| **identity-lock** | Provider-agnostic locked character/brand/persona identity, resolved through Relay for consistent generation. Source path: `apps/context-passport`. | runnable slice |
| **ledgerline** | Gross-margin lens: revenue + provider COGS on one Mint-backed ledger, settled through a durable outbox. | runnable slice |
| **game-economy** | Storefront for card packs, cosmetics and battle passes on Mint hold/capture. | runnable slice |
| **Switchyard** (`apps/toolgate`, working name) | The tool rail next to Relay: one `sk-t0-` key reaches a schema-typed catalog (our products first, free/public APIs second, BYO keys third) with Mint hold/capture, Rago events, SSRF-safe egress, an MCP server and the `sy` CLI. | MVP implemented; see below |

`apps/ranguard` (userspace integrity-monitor demo) and `apps/rithub-forge` (an optional Forgejo-based deployment, separate from Rithub) are not part of the rail.

## Switchyard (tool rail)

`apps/toolgate` is a standalone cargo workspace (`cd apps/toolgate && cargo test --workspace`; no root workspace member line is needed, same as Abacus and Rago). It serves `/v1/tools/*` (search by task, describe with price, call, batch), `/mcp` (streamable HTTP, OAuth 2.1 with PKCE, DCR, CIMD and the device flow for `sy login`), `/admin` and `llms.txt`. Keys are Relay's own `sk-t0-` keys, so one key covers models and tools. Paid calls hold on Mint before the upstream call and capture the exact cost; failures release the hold. Every call is written to a hash-chained audit log together with its Rago `tool_call` event in one transaction. See `apps/toolgate/README.md`.

## Consumed by
Menivor (`../videogen`) and other products call Railyard over HTTP + shared contract packages — they do **not** own it.

## Shared workstreams (from evaluation)
1. **Shared transactional datastore**: Turnstile and Abacus ship Postgres ownership + leased recovery; remaining datastore work is concentrated in the apps still using local/file state.
2. **Shared deploy plane**: Rago, Relay, Mint and Abacus co-deploy on one box behind Caddy; see `apps/DEPLOY-SHARED-PLANE.md`.

Relay prices each model at its own list price (111 rows as of 2026-09-24, including the OpenRouter free tier and Groq's published prices). It can also refresh those prices from OpenRouter's public model list on a timer (`RELAY_CATALOG_REFRESH_URL=openrouter`). Operator price edits always take precedence over the feed.
