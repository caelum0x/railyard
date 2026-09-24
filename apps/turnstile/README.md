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

Paid tools are **runtime-editable**: add, reprice, disable and delete them in
the console (`GET /admin`, Tools tab) or over the operator API, with no restart.
Every tool becomes a paid route over HTTP (`<http_method> /v1/tools/<name>/call`)
and MCP (`tools/call`), each priced independently. Works for upstreams written in
anything (Node, Python, Go).

Tools persist in Postgres (`turnstile_paid_tools`, migration `0004`) when
`TURNSTILE_DATABASE_URL` is set, otherwise in process memory (lost on restart).
Replicas pick up edits within `TURNSTILE_TOOLS_RELOAD_SECS` (default 5).

`TURNSTILE_TOOLS_PATH` is an optional **one-time seed**: a JSON array of tool
specs applied only while the store is empty, so console edits are never
overwritten by a restart:

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
  live in the store, the file or any API response.
- `http_method` (default `POST`; also `GET`, `PUT`, `PATCH`, `DELETE`) is the
  method agents call `/v1/tools/<name>/call` with, and the 402 challenge binds
  to it (a wrong method gets `405` with `Allow`). `upstream_method` accepts the
  same set. A `GET`/`DELETE` upstream gets the caller's query string (HTTP) or
  the flat scalar MCP arguments as query parameters instead of a body.
- The built-in `echo` demo tool (priced by `TURNSTILE_PRICE_MICROS`) is served
  only with `TURNSTILE_DEMO_ECHO=1` and shows as a read-only built-in row in the
  console. The server refuses to start only when it could never serve a tool:
  no stored or seeded tools, no demo flag and no admin token to add one.

#### Tool admin API (operator bearer token)

| Route | Purpose |
|---|---|
| `GET /v1/admin/tools[?include_deleted=1]` | every tool: spec, enabled, version, live status (`live`/`disabled`/`error`/`deleted`), credential env vars available, upstream policy |
| `POST /v1/admin/tools` | create (body = tool spec); `409` if the name is live; re-creating a deleted name restores it |
| `GET /v1/admin/tools/:name` | one tool |
| `PUT /v1/admin/tools/:name` | replace the spec; send `If-Match: <version>` to get `409` instead of overwriting someone else's edit |
| `POST /v1/admin/tools/:name/enable` / `/disable` | toggle |
| `DELETE /v1/admin/tools/:name` | soft delete: the route disappears, revenue and settlement history stay |
| `GET /v1/admin/tool-audit[?tool=&limit=]` | who changed what and when, with old and new values |

Writes get the same checks as the seed file (name, price, methods, timeout,
unique name, credential env var set on this server), and `422` with the reason
otherwise. Optional header `X-Turnstile-Operator: <name>` labels the audit entry.

Edit semantics: a reprice applies to new challenges; a challenge already issued
is honored at the price it quoted (the nonce store binds the exact challenge, and
the capture ceiling is the authorized amount). Disable/delete removes the route
immediately, so outstanding challenges for it can no longer be paid; calls
already admitted settle normally and recovery keeps working.

Guardrails for API-edited tools (a leaked admin token must not become an SSRF
proxy or a secret-exfiltration channel); unchanged values of an existing tool
are not re-checked:

- `TURNSTILE_UPSTREAM_ALLOW_HOSTS` — optional host allowlist (`api.example.com,*.example.com`).
- Private, loopback, link-local (incl. `169.254.169.254`), CGNAT and multicast
  upstreams, literal or DNS-resolved, are refused unless
  `TURNSTILE_UPSTREAM_ALLOW_PRIVATE=1`.
- `upstream_auth_env` must be `TURNSTILE_UPSTREAM_*` or listed in
  `TURNSTILE_UPSTREAM_AUTH_ENVS`; Turnstile's own secrets are never allowed.
  To add a new authenticated upstream, set its variable on every replica and
  restart once; afterwards all edits are live.

### Operator console and settlement access

- `TURNSTILE_ADMIN_TOKEN` (24+ chars) enables the console at `GET /admin` and:
  `GET /v1/admin/summary` (lifetime captured revenue, pending captures, per-tool
  revenue including retired tools), `GET /v1/admin/settlements?limit=&offset=&phase=`
  (paginated paid calls, with `has_more`/`next_offset`), `POST /v1/admin/recovery`
  (run a settlement recovery pass now, using the same lease protocol as the
  background reaper) and the tool admin API above. Console tabs: Tools (editor),
  Revenue, Paid Calls (with "Run recovery now"), Change History and Settlement Lookup.
- `GET /v1/settlements/:request_id` is scoped to the payer: present the exact
  `AITP-Authorization` you paid with (or the operator bearer token). A bare
  request id no longer reveals payer, amounts or the receipt.

`GET /healthz` is open and `POST /v1/echo` is paid. An unpaid call returns a
machine-readable AITP-402 challenge; a valid authorization is verified through
Mint and successful work is captured for actual usage.
The demo defaults to port `8101`, leaving Mint's default `8080` free.

The reference server also exposes:

- `GET /v1/tools` and `GET /.well-known/aitp-tools` — an OPEN discovery catalog
  of the provider's live priced tools (payee, currency, and price per tool),
  built from the same registry snapshot the gates quote so it can never drift.
- `POST /mcp` — a minimal JSON-RPC MCP endpoint exposing the SAME paid tools
  over the Model Context Protocol (`listChanged: true`; `GET /mcp` with
  `Accept: text/event-stream` streams `notifications/tools/list_changed` on
  every tool edit). Agents pay by putting their signed
  `AITP-Authorization` in the reserved `_aitp_authorization` tool argument; an
  unpaid `tools/call` returns a JSON-RPC error whose `data` carries the AITP-402
  challenge, and a paid call captures actual usage and returns the signed receipt
  in the result `_meta`.

**Turnstile** is the drop-in agent-paywall SDK + reference verifier — *"Stripe for
AI agents."* It makes any HTTP endpoint or MCP tool **paid-per-call in under 10
lines**: the calling agent pays with a fiat-backed, gasless, **success-only**
AITP-402 handshake, and the money settles on **Mint**.

**One-liner:** make any endpoint or MCP tool paid-per-call in <10 lines. The agent
pays with x402 (USDC) or AITP-402 (a fiat-backed, gasless Mint balance).
Either way the money settles on Mint, and only successful calls are charged.

## x402: the open HTTP 402 standard, next to AITP-402

Turnstile speaks **[x402](https://www.x402.org/)** (x402 Foundation; Coinbase
CDP and Stripe machine payments implement it) on every paid HTTP tool, in
addition to AITP-402. One `402` answers both protocols:

- **Body**: the x402 `PaymentRequiredBody` JSON: `{"x402Version":1,"error":...,
  "accepts":[{scheme:"exact", network, maxAmountRequired, resource, payTo, asset, ...}]}`.
  USDC has 6 decimals, the same scale as Railyard microdollars, so
  `maxAmountRequired` equals the tool's `price_micros`.
- **Headers**: `AITP-Challenge` + `AITP-Version`, unchanged, so AITP clients
  keep working.

An x402 client retries with `X-PAYMENT` (base64 JSON EIP-3009 authorization).
Turnstile then:

1. checks scheme, network, recipient (`payTo`), amount and expiry locally;
2. claims the authorization nonce in `turnstile_x402_payments` (primary key =
   replay guard, shared by every replica) **before** anything runs;
3. calls the facilitator `POST /verify`;
4. runs the tool. Only on a `2xx` does it call the facilitator `POST /settle`,
   so a failed tool never charges the payer (success-only billing, ADR-0002);
5. returns `X-PAYMENT-RESPONSE` (base64 `{success, transaction, network, payer}`);
6. credits the settled USDC into Mint with `POST /v1/mint`
   (`deposit_ref = usdc:<tx hash>`). Mint's USDC ramp re-reads the transaction
   receipt on-chain before minting, and idempotency is keyed on the tx, so a
   retry never double-credits. Credits that cannot confirm yet (chain not final)
   are retried every 30 s, up to 30 attempts.

If the settle call fails in transit, the payment is journaled as `unknown` and
the tool response is withheld (`503`). It is **not** retried blindly, because
the transfer may have landed. The operator reconciles it against the payer
address. `exact` moves a fixed amount, so an x402 call is charged the quoted
price; metered capture below the quote stays on the AITP/Mint-hold rail.

| Variable | Default | Notes |
|---|---|---|
| `TURNSTILE_X402_PAY_TO` | (unset = x402 off) | USDC receive address. To credit revenue into Mint, set this to the Mint `usdc` provider's `deposit_address` (treasury). |
| `TURNSTILE_X402_NETWORK` | `base-sepolia` | `base` for mainnet |
| `TURNSTILE_X402_FACILITATOR_URL` | `https://x402.org/facilitator` on Base Sepolia; **required** on mainnet | any x402 facilitator |
| `TURNSTILE_X402_FACILITATOR_AUTH` | (none) | sent verbatim as `Authorization` |
| `TURNSTILE_X402_ASSET` | USDC on Base / Base Sepolia | required on other networks |
| `TURNSTILE_X402_MINT_ACCOUNT` | (none) | Mint account credited with settled USDC |
| `TURNSTILE_PUBLIC_URL` | (none) | origin used for absolute `resource` URLs |
| `TURNSTILE_X402_MAX_TIMEOUT_SECS` | `60` | advertised `maxTimeoutSeconds` |

A half-configured rail stops the server at startup with the variable to fix.
Coinbase CDP's hosted facilitator signs each request with a short-lived JWT.
A static `TURNSTILE_X402_FACILITATOR_AUTH` cannot hold that, so on mainnet use
a facilitator that takes a static key, or run the open-source facilitator
yourself. Signing CDP JWTs per request is not built yet.

Endpoints: `GET /.well-known/x402` (free discovery of every tool's
requirements), `GET /v1/admin/x402/summary`, `GET /v1/admin/x402/payments`,
`POST /v1/admin/x402/credit` (operator token), and the **x402 Payments** tab
in `/admin`. MCP `tools/call` stays on AITP-402. MCP has no ratified x402
transport yet.

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
