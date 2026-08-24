# apps/turnstile

> **Status: runnable Rust SDK and reference server.**
> `apps/turnstile` is now its own cargo workspace (excluded from the repo-root
> GPUI/Metal graph) with three crates: `turnstile-core` (framework-agnostic
> AITP-402 paywall engine), `turnstile-axum` (drop-in axum middleware +
> `PaidRequest` extractor + 402/receipt responses), and `turnstile-demo` (a
> reference server: paid `POST /v1/echo` + open `/healthz`). It reuses the REAL
> `aitp-client` (path dep on `apps/agent-runtime`) for the wire types, header
> codec, and injectable `HttpTransport` seam — Turnstile forwards the agent's
> already-signed authorization to a live mint-server; it never re-signs.
>
> Build/test from here only: `cd apps/turnstile && cargo check` (or `cargo test`).

Run the paid echo product against Mint:

```bash
cd apps/turnstile
MINT_SERVER_URL=http://127.0.0.1:8080 \
TURNSTILE_API_KEY=mint-secret \
TURNSTILE_PAYEE=ait1qprovider \
TURNSTILE_PRICE_MICROS=4200 \
BIND_ADDR=0.0.0.0:8101 \
cargo run -p turnstile-demo
```

`GET /healthz` is open and `POST /v1/echo` is paid. An unpaid call returns a
machine-readable AITP-402 challenge; a valid authorization is verified through
Mint and successful work is captured for actual usage.
The demo defaults to port `8101`, leaving Mint's default `8080` free.

**Turnstile** is the drop-in agent-paywall SDK + reference verifier — *"Stripe for
AI agents."* It makes any HTTP endpoint or MCP tool **paid-per-call in under 10
lines**: the calling agent pays with a fiat-backed, gasless, **success-only**
AITP-402 handshake, and the money settles on **Mint**.

**One-liner:** make any endpoint or MCP tool paid-per-call in <10 lines; the agent
pays via AITP-402 and it settles on Mint — fiat-backed, gasless, success-only.

## Full plan

See the vision doc:
[`docs/apps-vision/turnstile.md`](../../docs/apps-vision/turnstile.md)

## What it reuses (no new rail — it rides the existing one)

- **Mint** (`apps/mint`, `docs/mint/*`) — the settlement rail. Turnstile is a
  first-class client of Mint's **AITP-402** protocol
  (`docs/mint/AITP-402-SPEC.md` §5): `402` challenge/authorize/verify,
  `aitp-hold` → `hold.create`/`capture`/`release` (success-only, ADR-0002),
  `aitp-transfer` → `transfer`, **allowances** (agent budgets), **hash-linked
  receipts**, **reserve 1:1 proof**, and Mint's exact-µ$ fee split. Turnstile holds
  **no money** — Mint is the vault.
- **Relay** (`apps/relay`) — the flagship AITP-402 reference integration; Turnstile
  generalizes Relay's 402 handling into a reusable middleware and dogfoods it back.
- **ReelForge** (`apps/api`) — Python provider-adapter discipline (injectable
  transport, fake in tests) for the Python SDK; render/publish/gateway ops become
  the first paid tools; clipmarket CPM payout rails reused for tool-author payouts.
- **Shared invariants** — ADR-0001 (integer microdollar) and ADR-0002
  (success-only billing); Neon/Postgres + Cloudflare for the thin metadata
  control-plane only.

## Current limits

The framework-agnostic engine and Axum adapter exist. Other framework adapters,
the MCP helper, registry/discovery, and published SDK packages do not. The demo's
nonce store is in memory, so horizontally scaled deployments need a shared nonce
store before they can safely accept a challenge on a different replica.
