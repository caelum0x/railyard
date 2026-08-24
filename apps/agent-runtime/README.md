# agent-runtime

Base agent runtime for the agent-economy apps (**Turnstile**, **Abacus**).
Agents run inference through **RELAY** (our OpenAI-compatible TensorZero gateway)
and pay via an **AITP-402** seam. Both an offline stub and the real
`MintPaymentGateway` are implemented; callers choose the transport at wiring time.

## Standalone workspace

This is its **own** cargo workspace and is listed in the repo-root `Cargo.toml`
`exclude` array (next to `apps/relay` and `apps/mint`) so it never triggers the
`apps/desktop` GPUI/Metal build. Build/test only from here:

```bash
cd apps/agent-runtime && cargo check   # or cargo test
```

Requires rustc >= 1.94 (see `rust-toolchain.toml`) because it path-depends on the
edition-2024 vendored rig crates.

## Crates

- **`aitp-client`** — the AITP-402 payment seam: wire/header codecs,
  `StubPaymentGateway`, Ed25519 authorization signing, and a reqwest-backed
  `MintPaymentGateway` for authorize→capture/release.
- **`relay-provider`** — `RelayModel`, a rig `CompletionModel` backed by Relay's
  OpenAI-compatible Chat Completions API (genuinely reuses vendored `external/rig`).
- **`agent-core`** — the runtime: `PaidCompletionModel` decorator wiring the
  payment seam onto any model, plus `build_agent` / `build_paid_agent` helpers
  that produce a rig `Agent`.

## rig dependency

PATH deps on the vendored MIT copy under `external/rig`, declared in
`[workspace.dependencies]`:

```toml
rig-core  = { path = "../../external/rig/crates/rig-core",  default-features = false, features = ["reqwest", "rustls"] }
rig-agent = { path = "../../external/rig/crates/rig-agent", default-features = false, features = ["derive"] }
```

`Agent`/`AgentBuilder`/`Prompt`/`Tool` live in `rig-agent` in 0.42; `rig-core`
holds the model/provider/completion layer. Tests add the `test-utils` feature to
pull in `MockCompletionModel` so nothing touches the network.

## Run it

```bash
cd apps/agent-runtime
cargo run -p agent-core --example minimal_agent
```

Run a real paid inference through Relay and Mint:

```bash
cd apps/agent-runtime
RELAY_URL=http://127.0.0.1:3000/openai/v1 \
RELAY_API_KEY=relay-secret \
RELAY_MODEL=openai::gpt-4o-mini \
MINT_SERVER_URL=http://127.0.0.1:8080 \
MINT_API_KEY=mint-secret \
MINT_PAYER_ACCOUNT=acct_customer \
MINT_PAYER_KEY_ID=key_1 \
MINT_PAYER_SIGNING_KEY='<64 hex chars>' \
AITP_PAYEE=ait1qprovider \
AITP_PRICE_MICROS=4200 \
AGENT_DIRECT_SETTLEMENT_ACK=1 \
AGENT_PROMPT='Summarize today in one sentence.' \
cargo run -p agent-core --example live_agent
```

The command prints the derived public key to stderr; register that tuple in
Mint's `MINT_PAYER_KEYS_JSON` before running. This example settles directly with
Mint, so the selected Relay route/API key must not also apply Relay billing;
`AGENT_DIRECT_SETTLEMENT_ACK=1` makes that operator decision explicit. The
current limit is fixed operator-configured per-call pricing. Dynamic consumption
of the Relay-issued 402 quote, tools, and multi-turn session policy remain work.
