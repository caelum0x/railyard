# Abacus

## Deployed (2026-10-28)

Live on railyard-box (`130.61.113.116`), `profile: abacus` in
`infra-box/stacks/railyard/docker-compose.yml` (this profile and its
`abacus.caddy` site were already prepared by an earlier session, same as the
2026-10-24 live-audit pass that proved the kill-switch/insufficient-funds
paths against a local throwaway Mint — this pass filled
`/etc/products/abacus.env`, wrote a real `/srv/railyard/abacus/agents.json`
(one real direct-hold agent, replacing the template placeholders) and a real
`/srv/railyard/abacus/pricing.json` (the `relay` provider rows, since
`ABACUS_PROVIDER=relay`/`UPSTREAM_BASE_URL=http://relay:3000` fronts the
already-live Railyard Relay), built the image and brought it up for the
first time). Container port 8100 -> host `127.0.0.1:3900` -> Caddy
`abacus.{$RAILYARD_DOMAIN}`. Own Postgres database `abacus` (role
`abacus_app`, password already reserved in `postgres.env` — confirmed it
still matched and the role/db already existed). `MINT_API_KEY` is the real,
already-live Mint service key every other sub-app on this box uses; the
agent's `agent_acct`/`treasury` and the provider's payout `payee` address are
three freshly created real Mint accounts (`POST /v1/accounts` +
`POST /v1/accounts/:id/addresses` against the live `mint.ereboslab.com`).

Verified live end-to-end against the real deployed container, both locally
and over the real public hostname: a real `POST /v1/proxy` priced within cap
but against the real zero-balance treasury correctly `402`s
`insufficient_funds` from Mint's real `/v1/holds` rejection — cross-checked
directly against `GET /v1/accounts/:id/balance` on `mint.ereboslab.com`
(identical `0`, same ledger, not a stub); a request priced over the agent's
`per_request_max_micros` (5,000,000 µ$) is killed with `allowance_exceeded`
with **zero** Mint calls (confirmed via `GET /v1/admin/overview`:
`open_holds_total: 0` before and after); a wrong `X-API-Key` is `401`; an
unknown `X-Abacus-Agent` is rejected; `GET /v1/admin/overview` with the real
`ABACUS_ADMIN_API_KEY` shows the real agent/budget config loaded correctly.

**Live (2026-10-28):** the public DNS A record (`abacus.ereboslab.com` ->
`130.61.113.116`, proxied) was created via `~/products/.env`'s
`CLOUDFLARE_DNS_API_TOKEN` (real, separate, write-scoped Cloudflare token;
the `cloudflare-api` MCP connection remains read-only). Caddy reloaded, real
Let's Encrypt cert obtained automatically. Verified live:
`https://abacus.ereboslab.com/healthz` -> `ok` (200); the real
`insufficient_funds` round trip above was re-run against the public hostname
with the identical result.

**Status: implemented standalone Rust workspace.** Its five crates compile and
the workspace suite passes: `abacus-core`, `abacus-mint-client`, `abacus-proxy`,
`abacus-testkit`, and `abacus-upstream`. Validate from `apps/abacus` with
`cargo test --workspace`.

The `abacus-proxy` binary is runnable. It imports agent/budget policy from JSON,
forwards to a real upstream, settles against a real Mint server, and projects
idempotent receipt views into Postgres when configured (or a restart-safe JSONL
audit log in explicit single-replica mode):

```bash
cd apps/abacus
cp -f config/agents.example.json /tmp/abacus-agents.json
MINT_SERVER_URL=http://127.0.0.1:8080 \
MINT_API_KEY=mint-secret \
ABACUS_API_KEY=proxy-secret \
ABACUS_ADMIN_API_KEY=operator-secret-at-least-24-chars \
UPSTREAM_BASE_URL=https://api.openai.com \
ABACUS_AGENT_CONFIG=/tmp/abacus-agents.json \
ABACUS_RECEIPT_LOG=/tmp/abacus-receipts.jsonl \
ABACUS_DATABASE_URL=postgres://localhost/abacus \
cargo run -p abacus-proxy
```

Replace the example treasury account and payee in the copied JSON with real Mint
identities before sending paid traffic.

Compute the derived key the template's example agent must present (see the
per-agent key contract below) with:

```bash
printf '%s' "agent_01ARZ3NDEKTSV4RRFFQ69G5FAV" \
  | openssl dgst -sha256 -hmac "proxy-secret" | awk '{print $2}'
```

That hex string — not `proxy-secret` — is the `X-API-Key` this agent sends on
`POST /v1/proxy` / `GET /v1/budget`.

Clients call `POST /v1/proxy` with `X-API-Key`, `X-Abacus-Agent`, and the
upstream `Authorization: Bearer ...` header. An agent reads its own remaining
budget (policy, live allowance headroom, reservations in flight) with
`GET /v1/budget` and the same `X-API-Key` + `X-Abacus-Agent` headers.

**`X-API-Key` on these two routes is per-agent, not the raw `ABACUS_API_KEY`.**
`ABACUS_API_KEY` is the operator-only master secret; it is NEVER handed to an
agent directly. Each agent instead gets its own derived key —
`hex(HMAC-SHA256(ABACUS_API_KEY, agent_id))` (see
[`auth::derive_agent_key`](crates/abacus-proxy/src/auth.rs)) — computed once by
the operator and distributed only to that agent. `require_api_key` recomputes
this per-agent key from the `X-Abacus-Agent` the caller claims and compares it
to the presented `X-API-Key`, so one agent's key can never authenticate as a
different `X-Abacus-Agent`: swapping the header without also knowing that
other agent's derived key is a `401`, not a cross-agent spend/read.

`GET /v1/receipts/:request_id` (an agent reading back its own settled
receipt) uses the identical per-agent derived key + `X-Abacus-Agent` pair —
**fixed 2026-11-06**: it used to accept the bare `ABACUS_API_KEY` directly
with no agent binding at all (a real, narrower instance of the same bug
class the two routes above were fixed for first); now it also enforces that
the receipt returned actually belongs to the agent that authenticated
(a mismatch reports the same `404` as "never existed", so an agent can't
enumerate other agents' receipts by probing request ids). The bare master
key is never accepted directly on any route, including this one — use
`ABACUS_ADMIN_API_KEY` for cross-agent/operator reads instead.

Operators use a SEPARATE key, `ABACUS_ADMIN_API_KEY` (min 24 chars, must differ
from `ABACUS_API_KEY`; unset disables the operator API): `GET /v1/admin/overview`
(every agent's policy, open holds, recent spend, allowance headroom),
`GET /v1/admin/receipts?agent_id=&limit=`, `GET /v1/admin/holds`, and the console
at `GET /admin`. `/readyz` returns `503` unless the hold store, the policy store
and Mint's own `/readyz` all answer. `ABACUS_BIND_ADDR` defaults to
`0.0.0.0:8100`; `ABACUS_PROVIDER` defaults to `openai`.

`ABACUS_PRICING_CONFIG` (optional) points at a JSON file of per-provider/model
rates that overlay the built-in OpenAI table, so Abacus can price ANY provider or
model instead of killing it with `model_not_priced`. See
[`config/pricing.example.json`](config/pricing.example.json); each entry is
`{ provider, model, input_micros_per_1k, output_micros_per_1k }` in µ$ per 1000
tokens. Config entries override same-key built-ins; a missing/negative rate or an
empty key fails startup. Unset ⇒ only the built-in OpenAI rates apply. The
pre-flight estimate reserves the MAX cost conservatively: it sums text across
plain and structured (vision) message content, reserves a per-part floor for each
non-text image/audio/file part, and counts tool-call payloads and tool/function
definitions — so multimodal and tool-calling requests are never under-reserved.

`ABACUS_DATABASE_URL` enables the shared, migrated Postgres product store:
transactional policy import, signed receipt projection, and every open Mint
hold. Recovery workers claim expired holds with leases; each persisted hold also
carries its agent/run/budget/payer/request context. After an idempotent Mint
release, Postgres deletes the worker-owned claim and projects the signed released
receipt in one transaction, so a crash cannot leave money recovered but evidence
silently absent. Without Postgres, atomic JSON snapshots remain restart-safe but
policy, receipts, and recovery are explicitly single-replica. Mint remains the
authoritative money ledger in either mode.

**One-liner:** An inline reverse proxy that gives every autonomous agent a hard,
cryptographically-receipted spend budget — and kills the request the microsecond it
would breach it, so a looping agent can never burn $40k overnight.

## Full plan
There is no separate `docs/apps-vision/abacus.md` in this repo — it was never
committed (confirmed via `git log --all`), so the old link here was dead. This
README (architecture, reuse map, deployment status, runtime contract above) is
the current source of truth for the product. Cross-portfolio history, status,
and next-steps tracking for this product lives in `plans/railyard.md` in the
owner's `~/products` workspace (outside this repo).

## What it reuses
- **Relay** (`apps/relay/crates/*`): `relay-server` (proxy body), `relay-router` /
  `relay-normalize` / `relay-providers` (any-provider metering), `relay-tokenize`
  (pre-flight cost estimate), `relay-keys` (identity/attribution),
  `relay-billing`, `relay-guardrails` (budget guardrail slot), `relay-analytics`.
- **Mint** (`apps/mint`, AITP-402): allowances (`allowance.grant/spend`),
  holds→capture/release (race-free enforcement gate), AITP-402 `aitp-hold`
  (sessionless agent payments), hash-linked signed **receipts** (system-of-record),
  reserve/conservation (solvency proof), agent/treasury accounts (ledger identity).
- **ReelForge** (`apps/api/app/domains/*`): `spend` + `routing` (policy engine
  seed), `gateway` (provisioning/usage/markup pattern), `observability` /
  `compliance` (attribution + audit UI), `wallet` (Mint client, first dogfood),
  `web` (console shell).
- **Shared:** ADR-0001 integer-microdollar money, ADR-0002 success-only billing;
  Neon/Postgres, Cloudflare edge, OTel. Standalone Rust workspace **excluded from
  the root Cargo workspace** (like `apps/relay` and `apps/mint`) so it never
  triggers the `apps/desktop` GPUI/Metal build.

## Runtime dependency

The current product uses Mint holds/capture/release and optionally allowance-backed
budgets. The Ax0→Ax4 sequence in the vision document is historical planning;
the executable above is the current state.
