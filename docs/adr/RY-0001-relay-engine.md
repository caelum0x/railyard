# RY-0001: Relay engine after the TensorZero archive

- **Status:** accepted with the recommended default. The owner confirms it as **OD-RY1**
  in the Wave D owner session (`products/OWNER-SESSION-WAVE-D.md`). Until the owner
  answers, the default below is in force; nothing in it spends money or removes code.
- **Date:** 2026-09-29
- **Package:** RY-RL1 (Wave D). Source: `gtm/PARITY-RAILYARD-INFRA-2026-09-29.md`,
  section 2.
- **Also satisfies:** the "upstream decision is recorded" part of VC9's acceptance
  (vibe-coder plan). VC9 does not repeat it.

## Context

Relay (`apps/relay`) is our AI gateway: Rally, Menivor and Scribase call it, and it
holds the money layer (per-key caps, holds, receipts, cost events to Rago, AITP). It is
a fork of TensorZero, Apache-2.0. The upstream repository `tensorzero/tensorzero` was
archived on 2026-06-11, so no upstream fixes will arrive, including provider-adapter
fixes and security fixes to its HTTP and TLS stack.

We use four provider adapters in production: **Anthropic, Groq, OpenRouter and
OpenAI** (`config/tensorzero.toml`). Every named route has OpenRouter as its second
leg, so a primary failure is hidden from customers by design. That also hides the
failure from us.

## Options considered

| | Keep the fork (engine frozen) | Bifrost as the provider layer | LiteLLM as the provider layer |
|---|---|---|---|
| Licence | Apache-2.0 (ours) | Apache-2.0 (incl. governance, semanticcache, otel, routing plugins) | MIT outside `enterprise/` |
| Upstream activity (gh, 2026-09-29) | archived 2026-06-11 | 8,422 stars, pushed 2026-09-28, Go | 59,799 stars, pushed 2026-09-28, Python |
| arm64 | yes, runs on box 2 today | yes (`maximhq/bifrost`) | yes (`ghcr.io/berriai/litellm`) |
| RAM | 30 MiB (measured) | 50-100 MB (est.) | 400-800 MB (est.) |
| Money layer | in place (`crates/relay-*`) | stays in Relay | stays in Relay |
| Migration cost | none now; we maintain 4 adapters | medium, config only: Relay routes a model to Bifrost through the `openai` provider type with `api_base` | medium, plus high box 2 RAM |
| Main risk | adapter drift, no upstream security fixes | one more hop, young project | RAM, Python supply chain, a proprietary `enterprise/` folder we must never enable |

## Decision (recommended default, OD-RY1)

1. **Keep the fork as the Relay engine.** The upstream surface is frozen. We maintain
   only the four adapters we use (Anthropic, Groq, OpenRouter, OpenAI). The other
   provider adapters stay in the tree, unchanged and labelled "not maintained" in
   this record. Nothing is deleted.
2. **Detect drift before users do.** A daily provider-contract canary
   (`apps/relay/tools/provider-canary/`, runner
   `infra-box/monitoring/canaries/relay-providers.sh`) calls every named chat route
   plain, streaming and with a tool call, and every fallback leg once. A route is green
   only when its **first configured provider** served the call, so a failover that
   still answers customers is red for us. It pages the ntfy `railyard` topic and
   pushes latency to VictoriaMetrics. Cost: about 34 calls a day (9 routes x 3 + 7 fallback legs, after RY-RL2) at 5 output tokens
   (48 for the tool call), on Claude credits and the Groq and OpenRouter tiers; about $0.
3. **Keep Bifrost ready as a provider layer (RY-RL4, Wave E).** Bifrost runs as a
   loopback sidecar behind a per-model flag. Money, caps and receipts stay in Relay.
   Relay's code does not change, only its config.
4. **LiteLLM is not deployed on either box.** Its MIT price JSON
   (`model_prices_and_context_window.json`) is used read-only as a price cross-check
   (RY-RL2). It never changes a price. `vcglass/lib/llm/litellm-gateway.ts` stays as it
   is (no callers).

## Tripwires

Any one of these moves the affected models, then all models, onto the Bifrost provider
layer (RY-RL4's switch). Relay stays the front door in every case.

1. The provider canary shows a break that is not fixed within 1 working day, twice in
   one quarter.
2. A provider feature we need exists in Bifrost and would take more than 2 days to port
   into the fork.
3. A security advisory affects the fork's HTTP or TLS stack and no patch lands within
   7 days.

The owner of the tripwires is the Railyard lane. Each canary page is logged in the
canary history (`/var/lib/infra-box/canaries/railyard.relay-providers.jsonl`, last 500
runs), which is the evidence for tripwire 1.

## Adapter maintenance scope

| Adapter | Status | Used by |
|---|---|---|
| `anthropic` | maintained | `claude-sonnet` (and `claude-haiku`, `claude-sonnet-5` from RY-RL2) |
| `groq` | maintained | `openai/gpt-oss-120b`, `openai/gpt-oss-20b`, the llama aliases, `functions.chat` default |
| `openrouter` | maintained | second leg of every named route; the paid floor |
| `openai` | maintained | `gpt-4o`, `gpt-4o-mini`, `text-embedding-3-small` (primary leg; no key today, so OpenRouter serves) |
| every other provider in `crates/tensorzero-core/src/providers/` | **not maintained** (kept, unchanged) | none |

## Consequences

- A broken or revoked provider key is found within one daily run, even while customers
  are served by the fallback.
- Upstream security fixes do not arrive. Tripwire 3 bounds that exposure to 7 days.
- Adding a named route means regenerating the canary route list
  (`provider_canary.py --write-routes`); a unit test fails until it is done.
- Relay stays at about 30 MiB on box 2. No new service in Wave D.

## Follow-ups

- RY-RL2 (done): named `claude-haiku` (4.5) and `claude-sonnet-5` routes; LiteLLM price
  cross-check report (`relay-price-crosscheck`, read-only).
- RY-RL4 (Wave E): Bifrost spike. Its numbers are added to this record.
- Owner: answer OD-RY1 in the Wave D session; create the canary key and push monitor
  (tool README, "Owner setup").
