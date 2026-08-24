# apps/mint — AI Token settlement layer

> **Status: implemented standalone Rust workspace.** Ten crates compile and the
> workspace test suite passes. It remains excluded from the root desktop Cargo
> workspace; run validation from `apps/mint` with `cargo test --workspace`.

## Run the product

For a disposable local ledger:

```bash
cd apps/mint
MINT_API_KEY=local-secret MINT_ALLOW_INMEMORY=1 cargo run -p mint-server
curl http://127.0.0.1:8080/healthz
```

Production requires `DATABASE_URL`, `MINT_API_KEY`, and a unique 32-byte
`MINT_SIGNING_KEY` encoded as hex. The server refuses to use its deterministic
development signer when Postgres is configured. AITP-402 also needs the payer
verification-key registry, supplied as a JSON array:

```bash
export MINT_PAYER_KEYS_JSON='[{"payer":"acct_customer","pubkey_id":"key_1","public_key_hex":"<64 hex chars>"}]'
```

The server runs migrations at startup. Payment on-ramp adapters are activated by
their provider environment variables; with none configured, deposit confirmation
uses the development ramp and must not be treated as production funding.

Mint turns AI credits into a first-class **transferable token (AIT)** —
sendable/receivable/deliverable between accounts, agents, providers, and models —
with **web3 mechanics and zero crypto** (addresses, signed transfers,
conservation-of-supply, verifiable hash-linked receipts; fiat-backed, off-chain,
no gas). It's meant to become the canonical settlement layer that ReelForge
(`apps/api`), **Relay** (`apps/relay`), and desktop all spend on.

## Design docs (source of truth)
- **[docs/mint/PLAN.md](../../docs/mint/PLAN.md)** — architecture, crates, phases, invariants, product surfaces.
- **[docs/mint/AITP-402-SPEC.md](../../docs/mint/AITP-402-SPEC.md)** — the wire protocol: AIT unit, addresses, ledger ops, the AITP-402 payment handshake, hash-linked receipts.
- **[docs/mint/IMPLEMENTATION.md](../../docs/mint/IMPLEMENTATION.md)** — build-grade detail: Postgres schema, per-phase task lists, wallet integration/migration, reserve reconciliation, fee math, threat model, compliance tiers, SLOs, conformance suite.

## Implemented crates
`mint-core` · `mint-ledger` · `mint-payments` · `mint-protocol` ·
`mint-receipts` · `mint-reserve` · `mint-server` · `mint-store` ·
`mint-store-postgres` · `mint-stream`

## Product gaps

Payer-key rotation is config-and-restart rather than an authenticated control
plane, KYC is still the permissive development gate, and production redeem/on-ramp
operations depend on configured providers. The longer-term custody, anchoring,
fee, and multi-currency decisions remain in `IMPLEMENTATION.md`.

## Product family
**ReelForge** (video) · **Relay** (inference gateway, `apps/relay`) · **Mint** (this).
All inherit ADR-0001 (integer-microdollar money) + ADR-0002 (success-only billing).
