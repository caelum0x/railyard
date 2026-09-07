# apps/mint — AI Token settlement layer

> **Status: implemented standalone Rust workspace.** Eleven crates compile and the
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

## Fund an account

`POST /v1/checkouts` creates a provider-hosted checkout for an existing Mint
account. The amount must be positive whole USD cents expressed as integer
microdollars. Mint binds the account ID into provider metadata; the provider's
verified webhook is the only operation that credits the account.

```bash
curl --fail-with-body http://127.0.0.1:8080/v1/checkouts \
  -H "Authorization: Bearer $MINT_API_KEY" \
  -H 'Content-Type: application/json' \
  -d '{"provider":"creem","account_id":"acct_customer","amount_micros":20000000}'
```

Configure at least one checkout-capable provider (`STRIPE_API_KEY`,
`CREEM_API_KEY`, `POLAR_ACCESS_TOKEN`, `PAYPAL_CLIENT_ID`, `USDC_RPC_URL` plus
`COINBASE_COMMERCE_API_KEY`, or `IYZICO_API_KEY`) and its corresponding webhook
secret. Mobile IAP providers confirm store purchases but do not expose this
arbitrary hosted-checkout route.

Mint turns AI credits into a first-class **transferable token (AIT)** —
sendable/receivable/deliverable between accounts, agents, providers, and models —
with **web3 mechanics and zero crypto** (addresses, signed transfers,
conservation-of-supply, verifiable hash-linked receipts; fiat-backed, off-chain,
no gas). It's meant to become the canonical settlement layer that ReelForge
(`apps/api`), **Relay** (`apps/relay`), and desktop all spend on.

Create a shareable receive address with
`POST /v1/accounts/{account_id}/addresses`, then pass the returned `ait1…`
value as `to` in `POST /v1/transfers`. An address is routing only: Mint resolves
it to the authoritative recipient before committing the ledger operation, while
the sender remains an authorized `acct_…` supplied by the trusted product
backend. Transfer responses contain the exact signed sender receipt plus a
privacy-safe `mint-transfer-attestation-v1`; exact idempotent replay returns the
same evidence. The Rust, Python, and TypeScript SDKs all require that proof
shape. The dynamic-language clients additionally reject a nominally successful
response when its sender, amount, operation, or journal sequence does not match
the submitted transfer. Cryptographic acceptance still requires an
operator-pinned key from the public rotation document; discovery alone is not a
trust decision.

## Design docs (source of truth)
- **[docs/mint/PLAN.md](../../docs/mint/PLAN.md)** — architecture, crates, phases, invariants, product surfaces.
- **[docs/mint/AITP-402-SPEC.md](../../docs/mint/AITP-402-SPEC.md)** — the wire protocol: AIT unit, addresses, ledger ops, the AITP-402 payment handshake, hash-linked receipts.
- **[docs/mint/IMPLEMENTATION.md](../../docs/mint/IMPLEMENTATION.md)** — build-grade detail: Postgres schema, per-phase task lists, wallet integration/migration, reserve reconciliation, fee math, threat model, compliance tiers, SLOs, conformance suite.

## Implemented crates
`mint-core` · `mint-ledger` · `mint-payments` · `mint-protocol` ·
`mint-receipts` · `mint-reserve` · `mint-server` · `mint-store` ·
`mint-store-postgres` · `mint-stream` · `mint-sdk`

## Product gaps

Payer-key rotation is config-and-restart rather than an authenticated control
plane. Redemption fails closed until a real KYC provider is configured; no
allowlist or fake verification is used in production. Cash redemption also
depends on a payout-capable provider (Creem and mobile IAP are deposit-only).
The longer-term custody, anchoring, fee, and multi-currency decisions remain in
`IMPLEMENTATION.md`.

## Product family
**ReelForge** (video) · **Relay** (inference gateway, `apps/relay`) · **Mint** (this).
All inherit ADR-0001 (integer-microdollar money) + ADR-0002 (success-only billing).
