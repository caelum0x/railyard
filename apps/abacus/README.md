# Abacus

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

Clients call `POST /v1/proxy` with `X-API-Key`, `X-Abacus-Agent`, and the
upstream `Authorization: Bearer ...` header. An agent reads its own remaining
budget (policy, live allowance headroom, reservations in flight) with
`GET /v1/budget` and the same `X-API-Key` + `X-Abacus-Agent` headers.

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
See **[`docs/apps-vision/abacus.md`](../../docs/apps-vision/abacus.md)** for the
complete plan: problem/thesis, wedge + first customer, architecture, monetization,
moat, GTM, risks, phased roadmap (Ax0→Ax4), and success metrics.

## What it reuses (see the doc for specifics)
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
