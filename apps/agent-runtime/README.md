# agent-runtime

Base agent runtime for the agent-economy apps (**Turnstile**, **Abacus**).
Agents run inference through **RELAY** (our OpenAI-compatible TensorZero gateway)
and pay via an **AITP-402** seam. Relay owns holds and exact token-usage
settlement; the high-level Rig model signs one policy-approved `402` and retries
the identical request once.

## Standalone workspace

This is its **own** cargo workspace and is listed in the repo-root `Cargo.toml`
`exclude` array (next to `apps/relay` and `apps/mint`) so it never triggers the
`apps/desktop` GPUI/Metal build. Build/test only from here:

```bash
cd apps/agent-runtime && cargo check   # or cargo test
```

Requires rustc >= 1.94 as pinned by this workspace's `rust-toolchain.toml`. Rig
itself is the exact published open-source `rig-core` 0.39 dependency described
below; there is no hidden local Rig checkout.

## Crates

- **`aitp-client`** — the AITP-402 payment seam: wire/header codecs,
  policy-gated Ed25519 authorization signing, one-shot `402` retry, an offline
  stub, and a direct-Mint compatibility gateway.
- **`relay-provider`** — `RelayModel`, a rig `CompletionModel` backed by Relay's
  OpenAI-compatible Chat Completions API, including the AITP-aware Rig HTTP
  backend.
- **`agent-core`** — the runtime plus a clearly separate fixed-price direct
  compatibility decorator. Relay-billed models use `RuntimeAgent::new`.

The direct compatibility decorator supports both buffered and incremental Rig
streams. It authorizes a hold before asking the provider to create a stream,
captures the configured fixed price only after Rig yields a terminal provider
response, and releases on provider error or missing terminal state. Dropping a
consumer early schedules the same release on the current Tokio runtime. Once a
terminal response is observed, the command becomes capture permanently: an
ambiguous capture is retried idempotently and is never followed by release.
This path is intentionally fixed-price; Relay-backed models continue to use
Relay's model catalog and exact token-usage settlement instead.

## Rig open-source dependency

The workspace pins published MIT-licensed `rig-core` 0.39 exactly:

```toml
rig-core = { version = "=0.39.0", default-features = false, features = ["reqwest", "rustls", "derive"] }
```

The published crate supplies the agent, model/provider, completion and custom
HTTP-backend seams. Tests add `test-utils` for `MockCompletionModel`; paid
transport tests inject an in-memory Relay transport and never touch the network.

## Run it

```bash
cd apps/agent-runtime
cargo run -p agent-core --example minimal_agent
```

Run a real dynamically metered inference through Relay and Mint:

```bash
cd apps/agent-runtime
RELAY_URL=http://127.0.0.1:3000/openai/v1 \
RELAY_API_KEY=relay-secret \
RELAY_MODEL=openai::gpt-4o-mini \
MINT_PAYER_ACCOUNT=acct_customer \
MINT_PAYER_KEY_ID=key_1 \
MINT_PAYER_SIGNING_KEY='<64 hex chars>' \
MINT_VERIFIER_KEY_ID=mint-settlement-2026-01 \
MINT_VERIFIER_PUBLIC_KEY='<64 hex chars>' \
AITP_PAYEE=ait1qprovider \
AITP_MAX_AMOUNT_MICROS=100000 \
AGENT_PROMPT='Summarize today in one sentence.' \
cargo run -p agent-core --example live_agent
```

The command prints the derived public key to stderr; register that tuple in
Mint's payer-key store before running. Pin the settlement verifier id/public key
from the Mint operator's trusted configuration; the evidence response's own key
id is never used for trust discovery. The signer accepts only `AIT`, hold-mode
challenges for the configured Relay path/model, pinned payee, and amount at or
below `AITP_MAX_AMOUNT_MICROS`. Relay reserves that quote and captures its exact
TensorZero usage; the client never calls Mint directly and never supplies a
second price. The AITP-aware Rig backend supports buffered and genuinely
incremental SSE Chat Completions. A paid stream reaches clean EOF only after a
complete terminal usage frame and an authenticated, principal-scoped Relay
settlement lookup returns a receipt whose exact Mint attestation verifies against
that caller-pinned Ed25519 key. Missing usage/evidence, a body
disconnect, an application-level error, or a second `402` fails closed without
resubmitting inference. Completed stream proofs can be drained with
`RelayModel::drain_payment_evidence`; the evidence owner and funding payer are
kept as distinct identities. If a consumer drops before EOF, a bounded background
reconciler polls Relay without resubmitting inference. Pending ids remain visible
through `RelayModel::pending_payment_request_ids`, and shutdown/non-Tokio hosts
can invoke `RelayModel::reconcile_payment_evidence`. `RuntimeAgent::stream`
exposes Rig's high-level agent stream without bypassing this transport.

The same paid backend now supports Rig multipart forms. It encodes once, fixes
the boundary in `Content-Type`, and replays the byte-identical body through the
same one-challenge retry. The target Relay route still has to exist and be
covered by the caller's explicit resource policy.
