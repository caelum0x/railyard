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

Railyard client layer: `vcglass/lib/railyard/{config,errors,http,rago,relay,abacus,gtm-llm,chat-fetch,mint,store}.ts`
(env: `RAGO_URL`+`RAGO_API_KEY`, `RELAY_URL` or `RAILYARD_RELAY_URL`, `ABACUS_URL`+`ABACUS_API_KEY`, `MINT_BASE_URL`+`MINT_API_KEY`).

| Concern | Through Railyard (opt-in) | Still direct |
|---|---|---|
| Billing | AutoGTM usage metering + batch allowance via Rago: `lib/autogtm/rago-billing.ts` (`gtm_draft`, `gtm_email_sent`; used by `lib/autogtm/sending-worker.ts`, `app/api/autogtm/billing/route.ts`). Card top-ups emit `gtm_topup` when `RAGO_GTM_TOPUP_METERING=1` (off by default; `creditGtmBalance` in `lib/autogtm/usage-billing.ts`). Top-up audit trail on Mint: `lib/autogtm/mint-settlement.ts` | Charging customers is Creem: `lib/payments/creem.ts`, `lib/payments/products.ts`, `lib/vc-subscription.ts`; AutoGTM balance core `lib/autogtm/usage-billing.ts` |
| LLM | AutoGTM drafts `lib/autogtm/llm.ts` via `lib/railyard/gtm-llm.ts`; research/ask via `lib/railyard-llm.ts`. Every other chat call site uses `lib/railyard/chat-fetch.ts` (`llmChatFetch`, a drop-in `fetch` also passed as the OpenAI SDK `fetch`): with Relay off it is a pass-through (free Groq/OpenRouter chain unchanged); with Relay on the call goes to Relay with the model mapped to `tensorzero::model_name::<groq/openrouter/openai>::<model>` (or `RELAY_CHAT_MODEL`), body otherwise untouched, and Relay's cost feed meters it to Rago. Covers the OpenRouter factory `lib/openrouter.ts`, daily digest QA, query-plan, venture judge, entity-kind and sector classifiers, data-miner and crawl4ai extractors, funding contact extraction, trace rerank, vc-analyst synthesis, content-ops drafts, the OpenAI extraction service, and the private-market-search followups/prose/decompose/rerank/deep-search engines | Groq `compound-mini` (its web tools only exist on Groq; passes through by design); Anthropic SDK calls (`llm-prose-enrichment.ts`, Ask answering Anthropic path); embeddings (`lib/embeddings/adapter.ts`) and Voyage rerank; `lib/llm/litellm-gateway.ts` (no callers); eval/probe scripts |
| Payouts | none needed today | none |

Gaps:

1. Groq-gated call sites still return early when no Groq key is present, so a
   Relay-only deployment (no `GROQ_API_KEY*`) skips them instead of using Relay.
2. No embeddings path through Relay yet; Anthropic SDK calls bypass Relay.
3. Relay needs the Groq/OpenRouter provider keys and the Rago cost feed
   (`RELAY_COST_FEED_FORMAT=rago`) configured for the rerouted calls to be
   served and metered; unknown models fall back to the direct provider.
4. Rally's own subscription plans (VC viewer, Rally seats) are Creem-only with
   no Rago metering. Target: Creem stays the payment processor (merchant of
   record); plan usage events go to Rago.
5. `mint-settlement.ts` needs Mint accounts provisioned per project out of band
   (`railyardStore.getMintAccounts`); there is no UI to create them yet.
6. Abacus caps are wired for `railyard-llm`/`gtm-llm` only (not `chat-fetch`),
   and no per-project `ABACUS_AGENT_ID` mapping exists, so caps apply per
   deployment, not per customer.

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
`RAILYARD_RAGO_LLM_METRIC` (default `menivor_llm_call`, a count metric
seeded in `apps/rago/seeds/menivor-reels.json` at $0), `RAILYARD_RAGO_TIMEOUT_SECONDS`.

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

## Wave P switch table (PY1.5, dispatch 5.3)

One contract for Scribase, Menivor and Rally
(`tests/contract/consumers/contract.json`, tested against the seeds in
`apps/rago/seeds/portfolio/`). Nothing switches live in code: each product
reads `RAILYARD_LIVE` at deploy.

| Env (per product) | Kind | Filled by |
|---|---|---|
| `RELAY_URL` | var | deploy (box 2 internal URL) |
| `RELAY_KEY` | secret | `gen-secrets.sh` on the box, from `scripts/relay/provision-product-keys.mjs` |
| `RAGO_URL` | var | deploy |
| `RAGO_API_KEY` | secret | `gen-secrets.sh` (one Rago org per product) |
| `MINT_URL` | var | deploy |
| `MINT_API_KEY` | secret | `gen-secrets.sh` (a scoped Mint service key) |
| `RAILYARD_LIVE` | `0` or `1` | the deploy step, per product |

| Product | Meters (Rago `code`) | `RAILYARD_LIVE=0` | `RAILYARD_LIVE=1` |
|---|---|---|---|
| Scribase | `scribase.addon.slot_active`, `scribase.ai.tokens`, `scribase.analytics.events`, `scribase.push.sent`, `scribase.email.sent`, `scribase.purchases.tracked_revenue` (Wave Q, one event per organization and month) | counts stay in Scribase (`scribase.usage_*`, add-on rows) | the carve-out metering hook (`CA/adapters/railyard/metering.rs`) posts each event |
| Menivor | `menivor.reel.rendered`, `menivor.preset.render`, `menivor.lora.train`, `menivor.ott.gb_stored`, `menivor.ott.minutes_delivered`, `menivor.ott.encode_minutes`, `menivor.ai.minutes` | Menivor's own ledger only | PM1.5 posts each event |
| Rally | `rally.email.sent`, `rally.lead.revealed`, `rally.verification`, `rally.passthrough.domain`, `rally.passthrough.mailbox` | Rally's own counts only | PR1.5 posts each event |

Every event is `{ transaction_id: "<product>:<meter>:<source id>",
external_customer_id, external_subscription_id, code, timestamp, properties }`;
a repeated `transaction_id` is a no-op. Rago's `POST /events` requires
`external_subscription_id`, so each product customer gets one mirror
subscription `<product>-portfolio-<external_customer_id>` on the product's $0
portfolio plan (the dispatch named only `external_customer_id`; both are sent). The nightly `tools/reconcile-portfolio`
compares each product's own counts with Rago's totals and must show a
difference of 0. Mint keeps one mirror record per paid event (each product
posts what its Dodo webhook told it to `POST /v1/mirror/events` with its
`MINT_API_KEY`, which is its mirror key) and delivers it to the configured
endpoints as a signed `mirror.recorded` webhook; the mirror never touches the
ledger and never initiates a payout of third-party funds.
