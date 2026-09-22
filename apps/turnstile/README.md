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
TURNSTILE_DATABASE_URL=postgres://localhost/turnstile \
BIND_ADDR=0.0.0.0:8101 \
cargo run -p turnstile-demo
```

`GET /healthz` is open and `POST /v1/echo` is paid. An unpaid call returns a
machine-readable AITP-402 challenge; a valid authorization is verified through
Mint and successful work is captured for actual usage.
The demo defaults to port `8101`, leaving Mint's default `8080` free.

The reference server also exposes:

- `GET /v1/tools` and `GET /.well-known/aitp-tools` — an OPEN discovery catalog
  of the provider's priced tools (payee, currency, and price per tool), built
  from the same config the gate quotes so it can never drift.
- `POST /mcp` — a minimal JSON-RPC MCP endpoint exposing the SAME paid `echo`
  tool over the Model Context Protocol. Agents pay by putting their signed
  `AITP-Authorization` in the reserved `_aitp_authorization` tool argument; an
  unpaid `tools/call` returns a JSON-RPC error whose `data` carries the AITP-402
  challenge, and a paid call captures actual usage and returns the signed receipt
  in the result `_meta`.

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

## Durable challenge and settlement ownership

When `TURNSTILE_DATABASE_URL` is set, the reference server migrates and uses a
shared Postgres challenge store. Every nonce retains the exact issued
resource/payee/amount/scheme/expiry, so another replica can verify and consume
it exactly once. Altered authorizations do not burn the legitimate challenge.
The same database stores the exact verification command, Mint hold identity,
terminal direction, actual charge, signed receipt, owner token, and recovery
lease. The request renews its lease while the handler runs. If it disappears,
another replica atomically claims the command and:

- releases an abandoned verified hold;
- replays a pending capture with its original amount and idempotency identity;
- replays a pending release;
- re-verifies an ambiguous verification without running the handler again.

Capture and release are monotonic alternatives: after capture intent is durable,
no error path can switch that hold to release. A successful handler whose
capture is not confirmed returns `503`, `AITP-Settlement: capture_pending`, and
`AITP-Request-Id`; recovery continues the capture. A failed handler whose release
is pending keeps its original status and carries
`AITP-Settlement: release_pending`.

Without `TURNSTILE_DATABASE_URL`, the demo identifies itself as single-replica
and uses in-memory challenge and settlement stores with the same state machine.

## Current limits

The framework-agnostic engine, Axum adapter, shared Postgres challenge and
settlement stores, lease recovery worker, settlement-status API
(`GET /v1/settlements/:request_id`), provider registry/discovery
(`ToolRegistry`/`ToolCatalog` + `GET /v1/tools`), the framework-agnostic MCP
paywall helper (`turnstile_core::mcp` + a reference `POST /mcp` JSON-RPC
endpoint), and the reference server exist. Other framework adapters (Actix,
Tower-for-non-axum, Python/TS SDKs) and published SDK packages do not.
