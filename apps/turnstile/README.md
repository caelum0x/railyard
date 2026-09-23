# apps/turnstile

> **Status: runnable Rust SDK and reference server.**
> `apps/turnstile` is now its own cargo workspace (excluded from the repo-root
> GPUI/Metal graph) with three crates: `turnstile-core` (framework-agnostic
> AITP-402 paywall engine), `turnstile-axum` (drop-in axum middleware +
> `PaidRequest` extractor + 402/receipt responses), and `turnstile-server` (the
> production paid-tool gateway: config-declared paid tools over HTTP and MCP,
> settlement journal, operator console at `/admin`, `/healthz`; the built-in
> paid `POST /v1/echo` only runs with `TURNSTILE_DEMO_ECHO=1`). It reuses the REAL
> `aitp-client` (path dep on `apps/agent-runtime`) for the wire types, header
> codec, and injectable `HttpTransport` seam — Turnstile forwards the agent's
> already-signed authorization to a live mint-server; it never re-signs.
>
> Build/test from here only: `cd apps/turnstile && cargo check` (or `cargo test`).
>
> Container image (build context = repo root, because of the `aitp-client` path
> dependency): `docker build -f apps/turnstile/Dockerfile -t railyard/turnstile-server .`

Run the paid echo product against Mint:

```bash
cd apps/turnstile
MINT_SERVER_URL=http://127.0.0.1:8080 \
TURNSTILE_API_KEY=mint-secret \
TURNSTILE_PAYEE=ait1qprovider \
TURNSTILE_PRICE_MICROS=4200 \
TURNSTILE_DATABASE_URL=postgres://localhost/turnstile \
BIND_ADDR=0.0.0.0:8101 \
TURNSTILE_DEMO_ECHO=1 \
cargo run -p turnstile-server
```

### Put your own endpoints behind the paywall (no Rust)

Point `TURNSTILE_TOOLS_PATH` at a JSON registry and every entry becomes a paid
tool over HTTP (`POST /v1/tools/<name>/call`) and MCP (`tools/call`), each
priced independently. Works for upstreams written in anything (Node, Python, Go):

```json
[
  {
    "name": "summarize",
    "description": "Summarize a document.",
    "price_micros": 20000,
    "upstream_url": "https://internal.example.com/summarize",
    "upstream_auth_env": "SUMMARIZE_UPSTREAM_TOKEN",
    "upstream_auth_header": "authorization",
    "usage_header": "x-usage-micros",
    "input_schema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] }
  }
]
```

- A 2xx upstream response captures the quote (or the `usage_header` amount,
  capped at the quote); any other status or a transport error releases the hold,
  so the agent is never charged for a failed call.
- Upstream credentials are read from the named environment variable; they never
  live in the registry file.
- `http_method` (default `POST`; also `GET`, `PUT`, `PATCH`, `DELETE`) is the
  method agents call `/v1/tools/<name>/call` with, and the 402 challenge binds
  to it. `upstream_method` accepts the same set. A `GET`/`DELETE` upstream gets
  the caller's query string (HTTP) or the flat scalar MCP arguments as query
  parameters instead of a body.
- The built-in `echo` demo tool (priced by `TURNSTILE_PRICE_MICROS`) is served
  only with `TURNSTILE_DEMO_ECHO=1`. With neither `TURNSTILE_TOOLS_PATH` nor
  that flag set, the server refuses to start, so a misconfigured deploy cannot
  charge for an echo.

### Operator console and settlement access

- `TURNSTILE_ADMIN_TOKEN` (24+ chars) enables `GET /v1/admin/summary` (captured
  revenue, pending captures, per-tool revenue) and
  `GET /v1/admin/settlements?limit=&phase=` (recent paid calls) and
  `POST /v1/admin/recovery` (run a settlement recovery pass now, using the
  same lease protocol as the background reaper). `GET /admin` is the console
  over those endpoints, with a "Run recovery now" action on the Paid Calls tab.
- `GET /v1/settlements/:request_id` is scoped to the payer: present the exact
  `AITP-Authorization` you paid with (or the operator bearer token). A bare
  request id no longer reveals payer, amounts or the receipt.

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

Platform context: [`docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md`](../../docs/RAILYARD_ENTERPRISE_ARCHITECTURE.md)
and [`docs/CAPABILITY_GRAPH_2026-08-27.md`](../../docs/CAPABILITY_GRAPH_2026-08-27.md).

## What it reuses (no new rail — it rides the existing one)

- **Mint** ([`apps/mint`](../mint/README.md)) — the settlement rail. Turnstile is a
  first-class client of Mint's **AITP-402** protocol (implemented in
  `apps/mint/crates/mint-protocol`): `402` challenge/authorize/verify,
  `aitp-hold` → `hold.create`/`capture`/`release` (success-only, ADR-0002),
  `aitp-transfer` → `transfer`, **allowances** (agent budgets), **hash-linked
  receipts**, **reserve 1:1 proof**, and Mint's exact-µ$ fee split. Turnstile holds
  **no money** — Mint is the vault.
- **Relay** (`apps/relay`) — the flagship AITP-402 reference integration; Turnstile
  generalizes Relay's 402 handling into a reusable middleware and dogfoods it back.
- **Any HTTP service** — via the config-driven tool registry above, a Node,
  Python or Go endpoint is paywalled by the reference server without an SDK in
  that language.
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
endpoint), the config-driven upstream tool registry, the operator summary/list
APIs, the reference server, and agent-side payment clients for TypeScript
(`clients/typescript`) and Python (`clients/python`) exist. The clients have
no dependencies. They read `/v1/tools`, pay `402` / MCP `-32402` challenges
under a spend policy, sign with Ed25519 over Mint's BLAKE3 `op_hash`
(cross-checked byte for byte against `aitp-client`), and poll
`/v1/settlements/:request_id`. Native in-process middleware for other
provider frameworks (Actix, Express, FastAPI) does not exist yet; non-Rust
providers use the upstream proxy mode instead.
