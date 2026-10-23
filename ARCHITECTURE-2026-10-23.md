# Railyard — Technical Architecture (ground truth, 2026-10-23)

Companion to `STRATEGY-2026-10-23.md` (competitive positioning/user research —
not duplicated here) and `docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md` (target
crate-level plan). This doc is the *current, deployed* shape only, read from
the actual code and infra configs, not the target plan. `PLAN-2026-10-23-DEEP.md`
does not exist in this repo at time of writing — nothing to avoid duplicating.

## 0. Two hosting targets, not one

Railyard is split across two real infrastructures — confirmed from
`apps/DEPLOY-SHARED-PLANE.md`, `apps/*/wrangler.jsonc`, and
`infra-box/RUNBOOK-A1.md`:

- **Oracle `railyard-box` (130.61.113.116)**, ARM64, 2 OCPU/12 GB, Ubuntu 24.04
  — the Rust services (Rago, Relay, Mint, Abacus, Vend, Turnstile,
  game-economy), one shared Postgres 16, Gitea/Rithub (`git.ereboslab.com`),
  and GlitchTip/Faultline (`errors.ereboslab.com`), all behind one Caddy.
- **Cloudflare Workers (owner's Cloudflare account)** — Insights
  (`apps/insights-edge`, live at `insights.ereboslab.com`) and Console
  (`apps/console`, built, merged, **not yet deployed live** — see Risk #1).

No AWS anywhere (owner is banned from AWS/Amazon per standing memory); no
Kubernetes; no managed message broker — Postgres is also the queue substrate
(`pg_cron`, outbox-style tables) everywhere a queue is needed.

## 1. System diagram

```mermaid
flowchart TB
    subgraph Clients
        SDK[Consumer product code<br/>Rally/vcpeer, Menivor, OpenJev,<br/>Scribase, mobile apps, games]
        Browser[End-user browser<br/>tracker.js / rrweb]
    end

    subgraph CF["Cloudflare Workers (owner CF account)"]
        Insights["Insights Worker<br/>apps/insights-edge<br/>insights.ereboslab.com — LIVE"]
        OrgDO[("Durable Object<br/>OrgCollector<br/>per-org batching")]
        D1[("D1: railyard-insights<br/>events + replay_sessions")]
        R2[("R2: railyard-insights-replay<br/>rrweb chunks")]
        KV[("KV: RAILYARD_SITES<br/>site keys/plan caps")]
        Console["Console Worker (OpenNext)<br/>apps/console<br/>railyard.ereboslab.com<br/>BUILT, merged to main,<br/>NOT deployed live"]
    end

    subgraph Box["Oracle railyard-box 130.61.113.116 (Caddy front door)"]
        Abacus["Abacus :3900<br/>POST /v1/proxy<br/>spend-cap reverse proxy"]
        Relay["Relay :3100<br/>apps/relay, TensorZero fork<br/>OpenAI-compatible gateway"]
        Mint["Mint :3400<br/>apps/mint<br/>ledger: holds/captures/<br/>Ed25519 receipts"]
        Rago["Rago :3300<br/>apps/rago, Lago-compatible<br/>usage metering/invoicing"]
        Vend["Vend :3600<br/>white-label LLM resale"]
        Turnstile["Turnstile :3700<br/>AITP-402 paywall"]
        GameEcon["game-economy :3800<br/>storefront/IAP credits"]
        Rithub["Rithub (Gitea fork)<br/>git.ereboslab.com — LIVE"]
        Faultline["Faultline (GlitchTip)<br/>errors.ereboslab.com — LIVE"]
        PG[("Shared Postgres 16<br/>one DB per service:<br/>rago mint relay vend<br/>turnstile game_economy abacus")]
    end

    subgraph Ext["Third-party providers"]
        LLMProv[("Groq / OpenRouter /<br/>Anthropic / OpenAI")]
        PayProv[("Stripe, Creem, Dodo,<br/>Polar, PayPal, iyzico,<br/>RevenueCat, Adapty, USDC")]
        Scribase[("Scribase auth<br/>api.scribase.com")]
    end

    Browser -- "POST /api/send (beacon)" --> Insights
    Insights --> OrgDO --> D1
    Insights --> R2
    Insights <--> KV
    Insights -. "RAGO_URL (empty today,<br/>wired not switched on)" .-> Rago
    Console <--> Scribase
    Console -. "INSIGHTS_API_URL / RELAY_URL<br/>(empty today)" .-> Insights
    Console -. "" .-> Relay

    SDK -- "LLM call,<br/>Authorization: Bearer key" --> Abacus
    Abacus -- "1 price MAX, hold" --> Mint
    Abacus -- "2 forward verbatim" --> Relay
    Relay --> LLMProv
    Relay -- "cost feed (opt-in)" --> Rago
    Abacus -- "3 capture EXACT or release" --> Mint
    Abacus -- "4 signed receipt" --> Mint

    SDK -- "usage events" --> Rago
    Rago --> PayProv
    Mint --> PayProv
    GameEcon --> Mint
    Vend --> Mint
    Turnstile --> Mint

    Abacus --> PG
    Relay --> PG
    Mint --> PG
    Rago --> PG
    Vend --> PG
    Turnstile --> PG
    GameEcon --> PG
```

## 2. Data-flow for the 2-3 most important user actions

### 2.1 Insights pageview/event ingestion (the only live self-serve flow)

1. End-user's browser loads `tracker.js` served by the Insights Worker;
   fires `POST /api/send` (Umami-compatible) on page load/route change.
2. Worker (`apps/insights-edge/src/index.mjs`) checks `dropReason` (bot/prefetch
   filter), looks up the site key in **KV** (`RAILYARD_SITES`, 1-minute
   in-memory cache), computes a **daily-salted visitor hash** so the raw IP
   never leaves the function (`visitor.mjs`) and checks the plan cap
   (`caps.ts`).
3. Event is forwarded to that org's **Durable Object** (`OrgCollector`,
   `collector.mjs`) which batches on a 2-second flush.
4. On flush, the DO writes the batch to **D1** (`railyard-insights` database —
   this is the actual $0 event store) and, for session replay, chunks go to
   **R2** (`railyard-insights-replay`, one object per `<siteKey>/<sessionId>/<seq>.json`).
5. Dashboard reads (`store.mjs`) query D1 directly behind a tokenized link
   (email+domain signup, no password) — this is the one step-3/step-4
   "self-serve activation" and "first value moment" the strategy doc
   verified as proven end-to-end.
6. `RAGO_URL` and `PRODUCT_ANALYTICS_URL` vars are present in
   `wrangler.jsonc` but **set to empty string** — events are counted/capped
   locally but not forwarded to Rago yet; the weekly funnel report
   (Monday 07:00 UTC cron) goes out through Scribase Mail, not Rago.

### 2.2 A metered, success-only-billed LLM call (Abacus -> Relay -> Mint)

Named the "7-step hot path" in `apps/abacus/crates/abacus-proxy/src/lib.rs`:

1. Consumer product (Rally, Menivor, OpenJev — per `docs/CONSUMERS.md`)
   calls `POST /v1/proxy` on Abacus with its `Authorization: Bearer` key.
2. Abacus authenticates the ingress key, resolves the agent identity and
   budget policy (`auth.rs`, `identity.rs`).
3. Abacus **prices the MAX possible cost** and opens a **Mint hold**
   (reserve gate, `enforce.rs` -> Mint `/v1/holds`) — this is the money
   movement that happens *before* any provider call, and it is a hold, not
   a charge.
4. Abacus forwards the request **verbatim** to Relay
   (`UPSTREAM_BASE_URL=http://relay:3000`), which is a TensorZero fork
   routing to Groq/OpenRouter/Anthropic/OpenAI depending on configured
   provider keys; an unconfigured provider 503s `provider_not_configured`.
5. On a 2xx response with real usage, Abacus **captures the EXACT actual
   cost** on Mint; on failure, it **releases the hold** — zero Mint movement
   for a failed call (`settle.rs`, ADR-0002, the structural claim the
   strategy doc's differentiation section leans on).
6. A signed Mint receipt is written as the system-of-record
   (`receipt.rs`/`audit.rs`); Relay optionally pushes a cost-feed event to
   Rago (`RELAY_COST_FEED_URL`, opt-in, off by default per `docs/CONSUMERS.md`).
7. All of Abacus's own state (policy, open holds, receipts) lives in its own
   Postgres database (`abacus` DB on the shared `railyard-box` Postgres) —
   metadata/projection only; Mint's Postgres (`mint` DB) is the only balance
   source of truth, per architecture invariant #4.

### 2.3 Console sign-up / trial (built, not reachable today)

1. User hits `railyard.ereboslab.com`, signs in via Google/GitHub/email
   through **Scribase** (`SCRIBASE_URL` pointed at `api.scribase.com`).
2. A $1 refundable card check and 14-day trial start through **Dodo**
   (`DODO_API_BASE=https://test.dodopayments.com` — still test mode in
   `wrangler.jsonc`).
3. Nav bundles Insights/Flags/Product analytics/Usage/Billing, reading
   `INSIGHTS_API_URL` and `RELAY_URL` — **both vars are empty strings** in
   the live config today, so even if the Worker were deployed, those panels
   have nothing to call.
4. **This flow cannot complete at all right now**: `lib/config.ts` throws
   (500s every route, confirmed live by a rollback test, `apps/console/NOTES.md`)
   whenever `SCRIBASE_URL === SCRIBASE_CONTROL_PLANE_HOST`, which is exactly
   today's baked-in value, because the dedicated `railyard-console` Scribase
   project has never been provisioned.

## 3. Real architectural risks / bottlenecks (current scale, zero budget)

- **The one unified product surface is undeployed.** Console is merged to
  `main` but `lib/config.ts`'s own safety guard 500s every route (including
  `/`) until a real `railyard-console` Scribase project exists and
  `SCRIBASE_URL` is repointed — an owner-only signup step, not a code or
  tooling blocker (confirmed: `wrangler deploy` and the Cloudflare OAuth
  token both work from this machine today).
- **Single shared Postgres, single 2 OCPU/12 GB box, no replica.** Every
  Rust service's database (rago, mint, relay's observability tables, vend,
  turnstile, game_economy, abacus) is one Postgres 16 instance on
  `railyard-box`. There is no read replica, no PITR beyond nightly
  `pg_dumpall` via restic, and no documented failover — a single box
  failure takes down the ledger (Mint), the billing engine (Rago) and the
  model gateway (Relay) simultaneously. At current near-zero production
  traffic this is an acceptable risk, but it has zero headroom for the
  "many payers" thesis the README states as the point of the repo.
  Mitigates the zero-budget constraint (one box is $0 marginal cost) at the
  price of a single point of failure for every rail. (`pg_cron` on this same
  Postgres is also required specifically for Relay's observability tables —
  one more reason it cannot simply be dropped to cut risk.)
- **No cross-service event bus — HTTP calls and opt-in cost feeds only.**
  Relay -> Rago, Insights -> Rago, and every consumer's metering path is a
  best-effort HTTP POST gated by an env var that defaults to off/empty
  (`RAGO_URL=""` in Insights, `RELAY_COST_FEED_URL` unset by default). A
  dropped request is a silently lost metering event unless the nightly
  `tools/reconcile-portfolio` diff catches it — meaning real usage/revenue
  numbers depend on a daily batch reconciliation job rather than
  at-call-time guarantees. (Mint's own hold/capture/receipt path is fully
  transactional and durable — this risk is specifically about usage
  *metering* for billing visibility, not the settlement ledger itself.)
- **Two independent identity/auth systems with no bridge, and no single
  operator login either.** Scribase auth (Console) and Gitea's native
  accounts (Rithub) don't share a session — `apps/gitea/README.md` confirms
  "no Railyard-run OIDC/identity server exists yet" (`TODO(railyard-sso)`),
  so the strategy doc's "one bill, one sign-in" claim for Rithub is not true
  in the code today. The same gap exists for operators: Mint, Relay and
  Abacus each have their own separate admin token scheme (`MINT_API_KEY`,
  `TENSORZERO_API_KEY`, `ABACUS_ADMIN_API_KEY`).
- **Faultline is operationally identical to upstream GlitchTip Cloud.**
  It runs on the same box, same Postgres pattern, with no integration into
  Rago's usage meter yet — so even setting positioning aside, it adds one
  more unmonitored service on the single box with no differentiated
  failure-mode handling (e.g., no documented behavior if its own DB fills
  the shared Postgres disk).
- **Zero-budget means zero horizontal scale path today.** Both the batch
  box (`box`, 130.61.180.42) and the serving box (`railyard-box`,
  130.61.113.116) are single 2 OCPU/12 GB Oracle A1 Flex instances (the free
  ARM tier) — any real traffic spike (e.g., the comparison-page growth move
  actually landing) has no autoscaling and a documented 12 GB RAM ceiling
  shared across 7+ services at 256-512 MB each.

## 4. Prioritized next-build list

Ordered by effort-vs-impact for reliability and for unlocking the strategy
doc's growth moves.

1. **Unblock the Console deploy (lowest effort, highest leverage).**
   Provision the `railyard-console` Scribase project and repoint
   `SCRIBASE_URL` away from the control-plane host. This is one owner-side
   signup step (no code change needed — `wrangler deploy`/secrets are
   already proven working) and it is the single blocker standing between
   "zero unified product" and growth move #1 (the Insights-to-console
   cross-sell banner) existing at all.
2. **Wire the Insights -> Rago metering path live** (`RAGO_URL`,
   `PRODUCT_ANALYTICS_URL` are currently empty strings). Low effort — the
   integration code path already exists and is just gated by an env var —
   and it is the prerequisite for growth move #3 (folding Faultline into a
   shared usage meter) and for the console's Usage/Billing panel to show
   real numbers once deployed.
3. **Turn on the Relay cost-feed + per-model price catalog for at least one
   production consumer** (Rally or Menivor, both already have the client
   code per `docs/CONSUMERS.md`). Medium effort. This is the prerequisite
   for shipping the "$0 markup vs. OpenRouter's 5.5-8%" comparison page
   (growth move #2) with a real number instead of a theoretical one — right
   now Relay bills every model at one flat default per the README's own
   stated gap.
4. **Add a Postgres standby/PITR plan for `railyard-box`** beyond nightly
   `pg_dumpall`. Medium effort, meaningfully reduces the single-box blast
   radius identified in Risk #2 — even a same-box streaming replica or a
   more frequent WAL-archiving backup narrows the recovery window for the
   ledger (Mint) without adding a second paid box.
5. **Bridge Rithub into Scribase/Console auth (`TODO(railyard-sso)`).**
   Higher effort (needs an actual OIDC provider or a Scribase-issued token
   Gitea trusts) but is the one code change the strategy doc explicitly
   flags as required before "one bill, one sign-in" can be marketed for
   Rithub without being a false claim on day one.
