# Abacus

**Status: implemented standalone Rust workspace.** Its five crates compile and
the workspace suite passes: `abacus-core`, `abacus-mint-client`, `abacus-proxy`,
`abacus-testkit`, and `abacus-upstream`. Validate from `apps/abacus` with
`cargo test --workspace`.

The `abacus-proxy` binary is runnable. It loads agent/budget policy from a JSON
file, forwards to a real upstream, settles against a real Mint server, and writes
idempotent receipt views to a restart-safe JSONL audit log:

```bash
cd apps/abacus
cp -f config/agents.example.json /tmp/abacus-agents.json
MINT_SERVER_URL=http://127.0.0.1:8080 \
MINT_API_KEY=mint-secret \
ABACUS_API_KEY=proxy-secret \
UPSTREAM_BASE_URL=https://api.openai.com \
ABACUS_AGENT_CONFIG=/tmp/abacus-agents.json \
ABACUS_RECEIPT_LOG=/tmp/abacus-receipts.jsonl \
cargo run -p abacus-proxy
```

Replace the example treasury account and payee in the copied JSON with real Mint
identities before sending paid traffic.

Clients call `POST /v1/proxy` with `X-API-Key`, `X-Abacus-Agent`, and the
upstream `Authorization: Bearer ...` header. `ABACUS_BIND_ADDR` defaults to
`0.0.0.0:8100`; `ABACUS_PROVIDER` defaults to `openai`.

The file store is suitable for a single process and small deployments. The
remaining scale blocker is a shared transactional policy/receipt store for
multi-replica operation; Mint remains the authoritative money ledger in either
mode.

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
