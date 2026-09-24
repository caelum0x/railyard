# Railyard consumers: who calls vendors directly, who goes through Railyard

Snapshot 2026-09-25. Railyard exists first to run billing, LLM routing,
metering and payouts for OUR portfolio (Rally, Menivor, Scribase, OpenJev, the
mobile apps and games), and second to sell the same services to others on the
same code path. This map lists, per product, where it still talks to vendors
directly and where it already goes through Railyard, with file references, and
the target wiring.

Every Railyard hop in every consumer is opt-in and env-gated: with the env
unset the product keeps its direct path. "Wired" below means the code exists
and is tested; none of it is switched on in production yet (that needs the
Railyard services deployed and the env set).

## Services referenced

| Service | What it does for consumers | Consumer-facing surface |
|---|---|---|
| Rago (`apps/rago`) | Lago-compatible usage metering, plans, invoices | `POST /api/v1/events`, customers, subscriptions, `current_usage`; seeds in `apps/rago/seeds/{autogtm,menivor,menivor-reels,opentypesafe}.json` |
| Relay (`apps/relay`, TensorZero fork) | OpenAI-compatible LLM gateway, per-model routing, observability | `<relay>/openai/v1/chat/completions`, model names `tensorzero::model_name::<provider>::<model>` |
| Abacus (`apps/abacus`) | Per-key LLM spend caps in front of Relay | `POST /v1/proxy` (402 = cap exceeded) |
| Mint (`apps/mint`) | Ledger, holds/captures, transfers, signed receipts; on-ramps (Stripe, Creem, Dodo, Polar, PayPal, iyzico, USDC, RevenueCat, Adapty) and payouts | `/v1/holds`, `/v1/transfers`, `/v1/deposits/:provider/webhook` |
| game-economy (`apps/game-economy`) | Game storefront: packs, cosmetics, battle pass, IAP currency credits | [`apps/game-economy/docs/HTTP-API.md`](../apps/game-economy/docs/HTTP-API.md), TS client `apps/game-economy/clients/ts/game-economy.ts` |
| Vend (`apps/vend`) | White-label LLM API (wallets, per-model prices, revenue splits) | for customers who resell LLM access |
| Turnstile (`apps/turnstile`) | AITP-402 paywall for paid tools/APIs | not a rate limiter; only for paid tool endpoints |
| Identity Lock (`apps/context-passport`) | Versioned AI character/brand identity across providers | for Menivor characters later |

## Rally (vcpeer.com, repo `vcglass`)

Railyard client layer: `vcglass/lib/railyard/{config,errors,http,rago,relay,abacus,gtm-llm,mint,store}.ts`
(env: `RAGO_URL`+`RAGO_API_KEY`, `RELAY_URL`, `ABACUS_URL`+`ABACUS_API_KEY`, `MINT_BASE_URL`+`MINT_API_KEY`).
Verified by reading; not edited in this package.

| Concern | Through Railyard (opt-in) | Still direct |
|---|---|---|
| Billing | AutoGTM usage metering + batch allowance via Rago: `lib/autogtm/rago-billing.ts` (used by `lib/autogtm/sending-worker.ts`, `app/api/autogtm/billing/route.ts`). Top-up audit trail on Mint: `lib/autogtm/mint-settlement.ts` | Charging customers is Creem: `lib/payments/creem.ts`, `lib/payments/products.ts`, `lib/vc-subscription.ts`; AutoGTM balance core `lib/autogtm/usage-billing.ts` |
| LLM | AutoGTM drafts `lib/autogtm/llm.ts` via `lib/railyard/gtm-llm.ts`; research/ask via `lib/railyard-llm.ts`: `lib/ask-answering.ts`, `lib/ask/vc-analyst-synthesis.ts`, `lib/ask/streaming-summary.ts`, `lib/crawl4ai/llm-extract.ts`, `lib/crawl4ai/perplexity-synthesizer.ts`, `lib/research-judge/venture-answer-judge.ts`, `lib/content-ops/draft-provider.ts` | OpenRouter/Groq/OpenAI clients that bypass `railyard-llm`: `lib/ask/engines/groq-compound-engine.ts`, `lib/daily-digest-groq.ts`, `lib/research/query-plan.ts`, `lib/research/venture-judge.ts`, `lib/entity-kind-classifier.ts`, `lib/data-quality/classify-sector-llm.ts`, `lib/data-miner/llm-extractor.ts`, `lib/scoring/signals-osint.ts`, `lib/llm/model-router.ts`, `lib/llm/litellm-gateway.ts`, `lib/search/rerank/rerank-chain.ts`, `lib/embeddings/adapter.ts`, `lib/data-sources/funding-contact-enrichment.ts` |
| Payouts | none needed today | none |

Gaps (document only; Rally is being edited by other agents):

1. About 13 LLM call sites above skip `lib/railyard-llm.ts`, so turning on
   `RELAY_URL` only covers part of Rally's AI spend. Target: route them through
   `railyard-llm` (chat) and add an embeddings path through Relay.
2. Rally's own subscription plans (VC viewer, Rally seats) are Creem-only with
   no Rago metering. Target: Creem stays the payment processor (merchant of
   record); plan usage events go to Rago so our portfolio and outside customers
   are billed the same way.
3. `mint-settlement.ts` needs Mint accounts provisioned per project out of band
   (`railyardStore.getMintAccounts`); there is no UI to create them yet.
4. Abacus caps are wired but no per-project `ABACUS_AGENT_ID` mapping exists,
   so caps apply per deployment, not per customer.

## Menivor (repo `videogen`, Python API `apps/api`)

| Concern | Through Railyard (opt-in) | Still direct |
|---|---|---|
| Billing / credits | Credit holds/captures on Mint: `app/domains/wallet/mint_client.py`, `app/domains/reels/wallet_bridge.py`. Settlement metering to Rago: `app/domains/wallet/rago_client.py` (`RAGO_URL`, metric `reel_ai_credits`). New: per-render + credits events via `app/integrations/railyard/metering.py` (`RAILYARD_RAGO_*`), hooked in `wallet_bridge._meter_settlement` | Top-ups are Creem (or Stripe) checkouts: `app/domains/wallet/service.py` (checkout session), `app/domains/wallet/webhooks.py`. Media gateway jobs (`app/domains/gateway/media_gateway.py::_settle_job`) are not metered per render |
| LLM | `app/services/llm.py::_generate_response` OpenAI-compatible branch uses `app/domains/railyard/llm_gateway.py` (Abacus -> Relay -> direct) when `RELAY_URL` or new alias `RAILYARD_RELAY_URL` is set (`app/integrations/railyard/relay.py`); each Relay-routed call is metered (`menivor_llm_call`) | Other branches of `_generate_response` (Cloudflare AI Gateway, Azure, Gemini/Qwen/streaming, lines ~290-370), `app/services/voice.py` (MiMo TTS via OpenAI client), `app/domains/routing/providers/openrouter.py`, `app/domains/gateway/openai_image.py` (images) |
| Payouts / transfers | Wallet transfers to Mint addresses with attestations: `app/domains/wallet/{mint_accounts,destinations,transfer_attestation,transfer_reconciliation}.py` | none found |

Env for the new Menivor module (all optional; unset = no behavior change):
`RAILYARD_RELAY_URL`, `RAILYARD_RELAY_API_KEY`, `RAILYARD_RAGO_URL`,
`RAILYARD_RAGO_API_KEY`, `RAILYARD_RAGO_SUBSCRIPTION_ID` (portfolio mode) or
`RAILYARD_RAGO_SUBSCRIPTION_PREFIX` (customer mode: `<prefix><workspace or user id>`),
`RAILYARD_RAGO_RENDER_METRIC` (default `reel_generated`),
`RAILYARD_RAGO_CREDITS_METRIC` (default `reel_ai_credits`),
`RAILYARD_RAGO_LLM_METRIC` (default `menivor_llm_call`, needs a count metric
created in Rago; not in the seeds yet), `RAILYARD_RAGO_TIMEOUT_SECONDS`.

Target wiring: every Menivor LLM/image/TTS call through Relay (image and TTS
need Relay function configs); render-seconds metering (`reel_render_seconds`)
once the render pipeline reports duration at settlement; media gateway jobs
metered like reels.

## Scribase (repo `supabase-plus-clone`, `.../supabase/scribase`)

| Concern | Through Railyard | Still direct |
|---|---|---|
| Billing | none | Creem checkout, portal and signed webhook: `crates/control-api/src/adapters/billing_provider.rs` (`CREEM_API_KEY`, `SCRIBASE_BILLING_PRODUCT_*`); usage and capacity packs computed locally: `crates/control-api/src/adapters/{usage,billing}.rs` |
| LLM | none found in the control plane | none |
| Payouts | none | none |

Target: keep Creem as the processor; send project usage (DB size, egress,
function invocations) to Rago as events so plan limits and overage bills come
from one engine; when Scribase hosts our own apps (FlowBlast), meter those
projects against an internal portfolio subscription.

## OpenJev (repo `openjev`)

Railyard client layer already present: `src/lib/railyard/{config,errors,http,rago,meter,relay,abacus,llm,mint}.ts`,
env documented in `.env.example` (`RAGO_URL`/`RAGO_API_KEY`, `RELAY_URL`,
Mint optional); Rago seed `apps/rago/seeds/opentypesafe.json`.

| Concern | Through Railyard (opt-in) | Still direct |
|---|---|---|
| Billing | `systemone_call` event per decision via `src/lib/railyard/meter.ts` -> Rago (used by `src/routes/api/v1/{systemone,batch,usage}.ts`, `src/lib/jev/billing.server.ts`) | none found beyond the Rago path |
| LLM | escalation via `src/lib/railyard/relay.ts` when `RELAY_URL` is set | Provider adapters call vendors directly when Relay is off: `adapters/python/src/openjev_adapters/providers/{openai,anthropic}.py`, Groq/OpenAI keys in `.env.example` |

Target: nothing structural; turn on Relay in hosted OpenJev so escalation cost
is visible per customer.

## Mobile apps (Expo, sampled)

| App | Billing | LLM | Through Railyard |
|---|---|---|---|
| Echo (`echo/`) | RevenueCat SDK `echo/echo/src/services/purchases/revenuecat.ts`, server webhook `echo/server/src/lib/revenuecat.ts`, `echo/server/src/services/billing-service.ts` | Anthropic Messages API direct: `echo/server/src/services/ai-service.ts` | none |
| CalGym (`calgym/`) | RevenueCat, server events `calgym/apps/social-api/src/billing.ts` + migrations `0003_revenuecat_billing_events.sql` | not sampled | none |
| Calori AI (`calori-ai/`) | RevenueCat (coach subscriptions, `migrations/066_revenuecat_coach_subscriptions.sql`) | not sampled | none |
| Bump (`bump-app/`) | RevenueCat `lib/services/revenueCatService.ts`, `components/RevenueCatProvider.tsx` | none found | none |

Target for apps: RevenueCat stays the store SDK and receipt validator (App
Store/Play require native IAP). Point RevenueCat webhooks at Mint's
`/v1/deposits/revenuecat/webhook` only for apps that sell spendable balance;
subscription entitlements stay in RevenueCat. Server-side LLM calls (Echo's
`ai-service.ts`) move to Relay so every AI dollar in the portfolio shows up in
one place, with Abacus caps per app.

## Games (repo `games/`, sampled)

| Game | Billing | Through Railyard |
|---|---|---|
| FlowBlast (`games/flowblast`) | RevenueCat client `apps/mobile/src/iap/{revenuecat,entitlements,restore,skus}.ts`; entitlements table `infra/supabase/migrations/003_entitlements.sql`; economy schema `packages/schema/src/economy.ts` | none |
| Aqua Sort (`games/water-sort`) | RevenueCat one-time purchases `app/monetization/purchases.ts` | none |
| Block Puzzle (`games/block-puzzle`) | none found | none |

Target for games: RevenueCat keeps selling (native IAP), and game-economy
becomes the server-side store for spendable currency and goods:

1. The game backend (or FlowBlast on Scribase) issues player tokens with
   `POST /v1/tokens`.
2. Currency packs: RevenueCat webhook -> `POST /v1/iap/revenuecat` (grant
   amount from `GAME_IAP_PRODUCTS`) or Mint's RevenueCat on-ramp (grant = USD
   price). One path per product.
3. Packs, cosmetics and battle passes are bought with that balance through
   `POST /v1/purchase`, with signed Mint receipts and refunds/revocations.
4. The Expo app uses `apps/game-economy/clients/ts/game-economy.ts`
   (`GameEconomyPlayer`), the backend uses `GameEconomyServer`.

Remove-ads and other non-consumable unlocks can stay pure RevenueCat
entitlements; they do not need a ledger.

## Summary of target wiring

| Product | Billing processor | Metering | LLM | Ledger/payouts |
|---|---|---|---|---|
| Rally | Creem (MoR) | Rago (AutoGTM done; plans todo) | Relay/Abacus (partial) | Mint audit trail |
| Menivor | Creem/Stripe top-ups | Rago (settlement + per render done) | Relay (script path done; TTS/images todo) | Mint holds + wallet transfers |
| Scribase | Creem (MoR) | Rago (todo) | n/a | n/a |
| OpenJev | (hosted plans via Rago) | Rago (done) | Relay (done) | Mint optional |
| Apps | RevenueCat | Rago for server AI usage (todo) | Relay (todo) | Mint on-ramp for balance apps |
| Games | RevenueCat | game-economy + Mint receipts | n/a | Mint via game-economy |
